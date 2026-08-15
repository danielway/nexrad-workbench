//! Central canvas UI: radar visualization area.

use super::canvas_data_probe::{
    render_data_probe, render_distance_measurement, render_storm_cells,
};
use super::canvas_interaction::{handle_canvas_interaction, handle_globe_interaction};
use super::canvas_overlays::{
    draw_globe, draw_national_mosaic, render_alerts, render_chrome_overlays, render_gps_location,
    render_mping_detail, render_mping_reports, render_nexrad_sites, render_radar_sweep,
    AlertRenderPhase, OverlayContext, RadarCutout,
};
use super::colors::canvas as canvas_colors;
use crate::core::RenderProcessing;
use crate::geo::ViewMode;
use crate::geo::{GeoLayerSet, MapProjection};
use crate::nexrad::RadarGpuRenderer;
use crate::state::AppState;
use eframe::egui::{self, Color32, Rect, Sense, Stroke};
use geo_types::Coord;
use std::sync::{Arc, Mutex};

/// Render canvas with optional geographic layers and NEXRAD data.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_canvas_with_geo(
    root_ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    diagnostics: &mut crate::subsystem::Diagnostics,
    diagnostics_vm: &crate::core::diagnostics::DiagnosticsVm,
    derived: &crate::subsystem::Derived,
    geo_layers: Option<&GeoLayerSet>,
    gpu: &crate::GpuResources,
) {
    let gpu_renderer = gpu.gpu.as_ref();
    let globe_renderer = gpu.globe.as_ref();
    let geo_line_renderer = gpu.geo_line.as_ref();
    let globe_radar_renderer = gpu.globe_radar.as_ref();
    let volume_ray_renderer = gpu.volume_ray.as_ref();
    let ctx = root_ui.ctx().clone();
    egui::CentralPanel::default().show_inside(root_ui, |ui| {
        let available_size = ui.available_size();

        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        let dark = state.is_dark;

        // Draw background
        painter.rect_filled(rect, 0.0, canvas_colors::background(dark));

        match state.viz_state.view_mode() {
            ViewMode::Globe3D => {
                // Globe view doesn't have meaningful 2D lat/lon bounds;
                // downstream consumers should treat `None` as "unknown".
                state.viz_state.last_visible_bounds = None;

                // Update camera aspect ratio
                state.viz_state.camera.set_aspect(rect);

                // Advance any camera fly-to transition (pure core math;
                // the shell only supplies dt and keeps frames coming).
                let dt = ctx.input(|i| i.stable_dt.min(0.1));
                if state.viz_state.camera.tick_animation(dt) {
                    ctx.request_repaint();
                }

                // Draw the 3D globe via PaintCallback
                draw_globe(
                    ui,
                    &rect,
                    state,
                    globe_renderer,
                    geo_line_renderer,
                    gpu_renderer,
                    globe_radar_renderer,
                    volume_ray_renderer,
                );

                // Corner-chrome overlays (color scale + info + compass)
                // dispatched through the typed registry. The trait's
                // `visible` predicate keeps the scale bar (2D-only) out.
                render_chrome_overlays(
                    ui,
                    &OverlayContext {
                        rect,
                        state,
                        live,
                        derived,
                        diagnostics_vm,
                    },
                );

                // Handle orbit/zoom interactions
                handle_globe_interaction(&response, &rect, state);
            }
            ViewMode::Flat2D => {
                // --- Existing flat 2D path ---
                let mut projection =
                    MapProjection::new(state.viz_state.center_lat, state.viz_state.center_lon);
                projection.update(state.viz_state.zoom(), state.viz_state.pan_offset(), rect);

                // Cache current visible bounds so non-canvas UI (top bar chip,
                // modals) can filter geographic data without reconstructing
                // the projection.
                state.viz_state.last_visible_bounds = Some(projection.visible_bounds());

                // Camera-motion tracking for the label-tier debounce. The
                // projection fingerprint changes whenever pan, zoom, center,
                // or rect changes; we record the time of last change and
                // expose `camera_settled` so the label cache only rebuilds
                // after motion has stopped for `SETTLE_WINDOW_SECS`.
                let now_secs = derived.frame_now_secs;
                state
                    .render_cache
                    .camera_motion
                    .observe(projection.fingerprint(), now_secs);
                let camera_settled = state.render_cache.camera_motion.is_settled(now_secs);
                // Schedule exactly one wake-up at the settle moment so the
                // label tier rebuilds on time even when nothing else is
                // animating. No-op if already settled.
                if let Some(remaining) =
                    state.render_cache.camera_motion.time_until_settle(now_secs)
                {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(
                            (remaining * 1000.0).ceil().max(1.0) as u64,
                        ));
                }

                // Screen-space cutout circle for the active radar's coverage.
                // Fixed at the WSR-88D reflectivity range so the hole stays
                // stable as the user scrubs elevations/products instead of
                // tracking the current sweep's per-gate extent.
                const NEXRAD_MAX_RANGE_KM: f64 = 460.0;
                let radar_cutout = gpu_renderer.and_then(|renderer| {
                    let has_data = renderer.lock().expect("renderer mutex poisoned").has_data();
                    if !has_data {
                        return None;
                    }
                    let lon_range = crate::core::canvas::cutout_lon_range_deg(
                        state.viz_state.center_lat,
                        NEXRAD_MAX_RANGE_KM,
                    );
                    let center = projection.geo_to_screen(Coord {
                        x: state.viz_state.center_lon,
                        y: state.viz_state.center_lat,
                    });
                    let edge = projection.geo_to_screen(Coord {
                        x: state.viz_state.center_lon + lon_range,
                        y: state.viz_state.center_lat,
                    });
                    let radius = (edge.x - center.x).abs();
                    Some(RadarCutout { center, radius })
                });

                if state.layer_state.geo.national_mosaic && derived.data_is_live {
                    draw_national_mosaic(
                        &painter,
                        &projection,
                        &state.national_mosaic,
                        state.viz_state.zoom(),
                        radar_cutout,
                    );
                }

                if let Some(layers) = geo_layers {
                    crate::geo::render_geo_layers(
                        &painter,
                        layers,
                        &state.layer_state.geo,
                        &projection,
                        state.viz_state.zoom(),
                        state.layer_state.geo.labels,
                        crate::geo::GeoPass::Lines,
                        dark,
                        camera_settled,
                    );
                }

                render_nexrad_sites(
                    &painter,
                    &projection,
                    &state.viz_state.site_id,
                    &state.layer_state.geo,
                    crate::geo::GeoPass::Lines,
                );

                // Warning fills paint *under* the radar so storm data stays
                // readable over them; their outlines and the watch layer paint
                // over the radar below.
                let show_warnings = state.layer_state.geo.alerts_warnings;
                let show_other = state.layer_state.geo.alerts_other;
                let alerts_active = derived.data_is_live && !diagnostics.alerts.alerts.is_empty();
                if show_warnings && alerts_active {
                    render_alerts(
                        &painter,
                        &projection,
                        &diagnostics.alerts.alerts,
                        show_warnings,
                        show_other,
                        AlertRenderPhase::UnderRadar,
                    );
                }

                let sweep_info = compute_sweep_line_azimuth(state, timeline, playback);
                let (gpu_sweep, between_sweeps) =
                    compute_gpu_sweep_state(state, timeline, live, playback, derived, sweep_info);

                // Age desaturation needs the estimated antenna azimuth; only
                // supply it while the view's sweep animation is active so a
                // fixed elevation filter doesn't keep fading between visits.
                let chunk_boundary = derived
                    .effective_sweep_animation
                    .then_some(live.radar_model.estimated_azimuth)
                    .flatten();

                if let Some(renderer) = gpu_renderer {
                    // Live compositing keeps `gpu_sweep` set even when animation
                    // is off; force desaturation off in that case so age fades
                    // only accompany an active sweep animation (#134).
                    let mut processing = state.render_processing.clone();
                    processing.data_age_desaturation =
                        crate::core::canvas::data_age_desaturation_effective(
                            processing.data_age_desaturation,
                            derived.effective_sweep_animation,
                        );
                    draw_radar_gpu(
                        ui,
                        &projection,
                        renderer,
                        &rect,
                        state.viz_state.center_lat,
                        state.viz_state.center_lon,
                        &processing,
                        gpu_sweep,
                        chunk_boundary,
                    );

                    // Boundary ring at the edge of the active site's data range,
                    // matching the mosaic cutout so the seam reads as a single circle.
                    if let Some(c) = radar_cutout {
                        painter.circle_stroke(
                            c.center,
                            c.radius,
                            Stroke::new(1.5_f32, canvas_colors::ring_major(dark)),
                        );
                    }
                    // Request only as fast as the visible animation requires. A
                    // bare `request_repaint()` pins the UI at full display rate
                    // (often 60 fps) and compounds every per-frame loop below.
                    // When actual data is revealing (`gpu_sweep` / `between_sweeps`)
                    // or a live sweep is actively receiving radials, we want a
                    // smooth ~30 fps. When live is idle between sweeps we only
                    // need enough updates to advance the estimated-azimuth line
                    // (~10 fps is visually indistinguishable). Fully idle falls
                    // through to the 1 Hz global tick in `apply_frame_setup`.
                    let live_has_active_sweep = live
                        .radar_model
                        .active_sweep
                        .as_ref()
                        .is_some_and(|s| s.data_azimuth_range.is_some());
                    let live_has_moving_line =
                        live.radar_model.active && live.radar_model.estimated_azimuth.is_some();

                    if gpu_sweep.is_some() || between_sweeps || live_has_active_sweep {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(33));
                    } else if live_has_moving_line {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    }
                }

                if (show_warnings || show_other) && alerts_active {
                    render_alerts(
                        &painter,
                        &projection,
                        &diagnostics.alerts.alerts,
                        show_warnings,
                        show_other,
                        AlertRenderPhase::OverRadar,
                    );
                }

                if state.layer_state.geo.mping && !diagnostics.mping.reports.is_empty() {
                    render_mping_reports(
                        &painter,
                        &projection,
                        &diagnostics.mping.reports,
                        diagnostics.mping.window_min_ms,
                        diagnostics.mping.window_max_ms,
                        playback.state.playback_position(),
                        diagnostics.mping.selected_report_id,
                    );
                }

                // Labels pass: draw on top of radar + alerts so text stays
                // legible over bright reflectivity fills. Halo-stroked inside
                // the renderer for contrast.
                if let Some(layers) = geo_layers {
                    crate::geo::render_geo_layers(
                        &painter,
                        layers,
                        &state.layer_state.geo,
                        &projection,
                        state.viz_state.zoom(),
                        state.layer_state.geo.labels,
                        crate::geo::GeoPass::Labels,
                        dark,
                        camera_settled,
                    );
                }

                render_nexrad_sites(
                    &painter,
                    &projection,
                    &state.viz_state.site_id,
                    &state.layer_state.geo,
                    crate::geo::GeoPass::Labels,
                );

                if state.viz_state.storm_cells_visible
                    && !state.viz_state.detected_storm_cells.is_empty()
                {
                    render_storm_cells(
                        &painter,
                        &projection,
                        &state.viz_state.detected_storm_cells,
                        dark,
                    );
                }

                if state.layer_state.geo.gps_location {
                    if let Some(coords) = diagnostics.gps.coords {
                        render_gps_location(&painter, &projection, coords);
                    }
                }

                // Show sweep line when actively revealing, between sweeps, or during live streaming.
                // In live mode, the data boundaries and the "now" line are separate:
                //   data_sweep = (data_edge, data_start) — from actual received chunks
                //   now_line = estimated antenna position — what's currently being collected
                let (sweep_line_info, sweep_stale) = if live.radar_model.active {
                    // Use data boundaries for the donut arc (same as GPU compositing)
                    (gpu_sweep, false)
                } else {
                    match gpu_sweep {
                        Some((az, start)) if az != 0.0 || start != 0.0 => {
                            (Some((az, start)), false)
                        }
                        _ if between_sweeps => (state.viz_state.last_sweep_line_cache, true),
                        _ => (None, false),
                    }
                };
                render_radar_sweep(
                    &painter,
                    &projection,
                    state,
                    timeline,
                    live,
                    playback,
                    chrome,
                    derived,
                    sweep_line_info,
                    sweep_stale,
                );

                if state.viz_state.distance_tool_active || state.viz_state.distance_start.is_some()
                {
                    render_distance_measurement(
                        &painter,
                        &projection,
                        state.viz_state.distance_start,
                        state.viz_state.distance_end,
                    );
                }

                if state.viz_state.data_probe_enabled {
                    if let Some(hover_pos) = response.hover_pos() {
                        render_data_probe(
                            &painter,
                            &projection,
                            hover_pos,
                            state.viz_state.center_lat,
                            state.viz_state.center_lon,
                            gpu_renderer,
                            &state.viz_state.product,
                            state.use_local_time,
                            gpu_sweep,
                        );
                    }
                }

                if state.layer_state.geo.mping && diagnostics.mping.selected_report_id.is_some() {
                    render_mping_detail(
                        &painter,
                        rect,
                        &projection,
                        &diagnostics.mping.reports,
                        diagnostics.mping.selected_report_id,
                        state.viz_state.center_lat,
                        state.viz_state.center_lon,
                        playback.state.playback_position(),
                        state.use_local_time,
                    );
                }

                // Corner-chrome overlays (color scale + info + scale
                // bar) dispatched through the typed registry. The
                // trait's `visible` predicate keeps the compass
                // (3D-only) out.
                render_chrome_overlays(
                    ui,
                    &OverlayContext {
                        rect,
                        state,
                        live,
                        derived,
                        diagnostics_vm,
                    },
                );

                // Canvas honesty caption (spec §11.2). A blank canvas with a
                // fetch in flight reads as "loading", not "broken"; a held frame
                // whose time has drifted from the playhead surfaces the
                // discrepancy ("showing X · fetching/​no-data Y"). The target (Y)
                // is a PLAYHEAD time, kept visually distinct from the actual
                // collection readouts (info overlay) by the wording.
                render_canvas_caption(
                    &painter,
                    &rect,
                    ui.visuals().weak_text_color(),
                    state.viz_state.canvas_caption,
                    state.use_local_time,
                );

                // Mobile chrome reveal tap (spec §13): when the chrome was
                // hidden, the resolver latched this frame's press as a reveal.
                // Swallow it here so the same tap doesn't also pan/zoom the
                // map — the chrome is already coming back this frame.
                if !chrome.mobile_auto_hide.revealed_this_frame {
                    handle_canvas_interaction(
                        &response,
                        &rect,
                        state,
                        playback,
                        diagnostics,
                        derived,
                        &projection,
                    );
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_radar_gpu(
    ui: &mut egui::Ui,
    projection: &MapProjection,
    renderer: &Arc<Mutex<RadarGpuRenderer>>,
    rect: &Rect,
    radar_lat: f64,
    radar_lon: f64,
    processing: &RenderProcessing,
    sweep_info: Option<(f32, f32)>,
    sweep_chunk_boundary: Option<f32>,
) {
    // Check if renderer has data and get the actual data range
    let max_range_km = {
        let r = renderer.lock().expect("renderer mutex poisoned");
        if !r.has_data() {
            return;
        }
        r.max_range_km()
    };

    // Use the actual data max range (not a fixed constant) so the shader's
    // pixel-to-km mapping matches the geographic projection exactly.
    let range_km = max_range_km;

    let km_to_deg = 1.0 / 111.0;
    let lat_correction = radar_lat.to_radians().cos();
    let lon_range = range_km * km_to_deg / lat_correction;

    // Compute radar center in screen coordinates
    let center_screen = projection.geo_to_screen(Coord {
        x: radar_lon,
        y: radar_lat,
    });

    // Compute radius: distance from center to edge of coverage in screen pixels
    let edge_screen = projection.geo_to_screen(Coord {
        x: radar_lon + lon_range,
        y: radar_lat,
    });
    let radius_px = (edge_screen.x - center_screen.x).abs();

    let renderer = renderer.clone();
    let center = [center_screen.x, center_screen.y];
    let canvas_min = rect.min;
    let processing = processing.clone();

    let callback = egui::PaintCallback {
        rect: *rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            let gl = painter.gl();
            let r = renderer.lock().expect("renderer mutex poisoned");
            if r.has_data() {
                let px_per_point = info.pixels_per_point;
                let viewport = info.viewport_in_pixels();
                // Convert from screen points to physical pixels relative to viewport
                let adjusted_center = [
                    (center[0] - canvas_min.x) * px_per_point,
                    (center[1] - canvas_min.y) * px_per_point,
                ];
                r.paint(
                    gl,
                    adjusted_center,
                    radius_px * px_per_point,
                    [viewport.width_px as f32, viewport.height_px as f32],
                    &processing,
                    sweep_info,
                    sweep_chunk_boundary,
                );
            }
        })),
    };

    ui.painter().add(callback);
}

pub(super) fn format_age(secs: f64) -> String {
    if secs < 60.0 {
        format!("{}s", secs as u32)
    } else if secs < 3600.0 {
        let m = (secs / 60.0) as u32;
        let s = (secs % 60.0) as u32;
        format!("{}m{}s", m, s)
    } else if secs < 86400.0 {
        let h = (secs / 3600.0) as u32;
        let m = ((secs % 3600.0) / 60.0) as u32;
        format!("{}h{}m", h, m)
    } else if secs < 86400.0 * 365.0 {
        let d = (secs / 86400.0) as u32;
        let h = ((secs % 86400.0) / 3600.0) as u32;
        if d == 1 {
            format!("1 day {}h", h)
        } else {
            format!("{} days", d)
        }
    } else {
        let y = (secs / (86400.0 * 365.25)) as u32;
        let remaining_days = ((secs % (86400.0 * 365.25)) / 86400.0) as u32;
        if y == 1 {
            format!("1 year {} days", remaining_days)
        } else {
            format!("{} years", y)
        }
    }
}

pub(super) fn age_color(secs: f64) -> Color32 {
    if secs > ARCHIVE_AGE_THRESHOLD_SECS {
        Color32::from_rgb(255, 160, 40) // orange for archive data
    } else if secs > 300.0 {
        Color32::from_rgb(255, 80, 80)
    } else if secs > 60.0 {
        Color32::from_rgb(255, 200, 60)
    } else {
        Color32::from_rgb(80, 220, 100)
    }
}

pub(super) const ARCHIVE_AGE_THRESHOLD_SECS: f64 = 3600.0;

pub(super) const AGE_RANGE_COLLAPSE_SECS: f64 = 300.0;

#[allow(clippy::too_many_arguments)]
fn compute_gpu_sweep_state(
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    derived: &crate::subsystem::Derived,
    sweep_info: Option<(f32, f32)>,
) -> (Option<(f32, f32)>, bool) {
    // Live partial compositing only when the in-progress elevation matches
    // the user's filter (Latest accepts any cut). Otherwise other tilts would
    // keep driving the GPU sweep boundary and age desaturation between visits
    // of a fixed elevation. Archive animation still uses effective_sweep_animation.
    let selected_elev = state.viz_state.elevation_selection.elevation_number();
    let live_range = crate::core::canvas::live_compositing_azimuth_range(
        selected_elev,
        live.radar_model.collecting_elevation(),
        live.radar_model
            .active_sweep
            .as_ref()
            .and_then(|s| s.data_azimuth_range),
    );
    let playback_ts = playback.state.playback_position();
    let sweep_bounds = if live_range.is_none() && derived.effective_sweep_animation {
        timeline
            .scans
            .find_recent_scan(playback_ts, 15.0 * 60.0)
            .and_then(|scan| {
                let displayed_elev = state
                    .viz_state
                    .displayed
                    .as_ref()
                    .map(|d| d.identity.elevation_number);
                scan.sweeps
                    .iter()
                    .filter(|s| Some(s.elevation_number) == displayed_elev)
                    .rfind(|s| s.start_time <= playback_ts)
                    .or_else(|| {
                        scan.sweeps
                            .iter()
                            .find(|s| Some(s.elevation_number) == displayed_elev)
                    })
                    .map(|s| (s.start_time, s.end_time))
            })
    } else {
        None
    };

    let gpu_sweep = crate::core::canvas::select_gpu_sweep(
        live_range,
        derived.effective_sweep_animation,
        sweep_bounds,
        playback_ts,
        sweep_info,
    );

    // Cache the sweep position for between-sweep display (pure rule).
    let new_cache = crate::core::canvas::next_sweep_cache(
        state.viz_state.last_sweep_line_cache,
        gpu_sweep,
        derived.effective_sweep_animation,
    );
    state.viz_state.last_sweep_line_cache = new_cache;
    let between_sweeps = crate::core::canvas::between_sweeps(
        derived.effective_sweep_animation,
        gpu_sweep,
        new_cache,
    );

    (gpu_sweep, between_sweeps)
}

fn compute_sweep_line_azimuth(
    _state: &AppState,
    timeline: &crate::subsystem::Timeline,
    playback: &crate::subsystem::Playback,
) -> Option<(f32, f32)> {
    if playback.state.speed.timeline_seconds_per_real_second() > 30.0 {
        return None;
    }

    let ts = playback.state.playback_position();

    // Find the sweep the playhead is in, then interpolate its azimuth (pure).
    timeline
        .scans
        .find_scan_at_timestamp(ts)
        .and_then(|scan| scan.find_sweep_at_timestamp(ts))
        .and_then(|(_, sweep)| crate::core::canvas::sweep_line_azimuth(sweep, ts))
}

pub(super) fn format_time_short(ts: f64, use_local: bool) -> String {
    let p = super::time_format::parts(ts, use_local);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        p.hour, p.minute, p.second, p.millis
    )
}

pub(super) fn format_unix_timestamp(ts: f64, use_local: bool) -> String {
    let p = super::time_format::parts(ts, use_local);
    let zone = if use_local { "Local" } else { "UTC" };
    format!(
        "{:02}:{:02}:{:02}.{:03} {zone}",
        p.hour, p.minute, p.second, p.millis
    )
}

pub(super) fn format_unix_timestamp_with_date(ts: f64, use_local: bool) -> String {
    let p = super::time_format::parts(ts, use_local);
    let suffix = if use_local { "" } else { " UTC" };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}{suffix}",
        p.year, p.month, p.day, p.hour, p.minute, p.second, p.millis
    )
}

pub(super) fn format_age_compact(now_secs: f64, ts_secs: f64) -> Option<String> {
    let age = now_secs - ts_secs;
    if (0.0..1.5).contains(&age) {
        Some("(now)".to_string())
    } else if (0.0..300.0).contains(&age) {
        Some(format!("({})", format_age(age)))
    } else {
        None
    }
}

/// Compact 12-hour `h:MM` clock for the honesty caption (e.g. "2:41"), honoring
/// the local/UTC preference. Matches the spec's "showing 2:41 · fetching 2:51…"
/// wording (12-hour, no meridiem — two adjacent times share the AM/PM context)
/// and stays consistent with the 12-hour top-bar/overlay readouts.
fn format_hhmm(ts: f64, use_local: bool) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts * 1000.0));
    let (h24, m) = if use_local {
        (d.get_hours(), d.get_minutes())
    } else {
        (d.get_utc_hours(), d.get_utc_minutes())
    };
    let h12 = match h24 % 12 {
        0 => 12,
        h => h,
    };
    format!("{h12}:{m:02}")
}

/// Render the canvas honesty caption (spec §11.2). Centered, low-key text:
/// "Acquiring data…" on a blank canvas, or "showing X · fetching Y…" /
/// "showing X · no data at Y" when a held frame's time has drifted from the
/// playhead. No-op for [`crate::core::canvas::CanvasCaption::None`].
fn render_canvas_caption(
    painter: &egui::Painter,
    rect: &Rect,
    color: Color32,
    caption: crate::core::canvas::CanvasCaption,
    use_local: bool,
) {
    let text = match caption {
        crate::core::canvas::CanvasCaption::None => return,
        crate::core::canvas::CanvasCaption::Acquiring => "Acquiring data…".to_string(),
        crate::core::canvas::CanvasCaption::Discrepancy {
            showing,
            target,
            fetching,
        } => {
            let showing_s = format_hhmm(showing, use_local);
            let target_s = format_hhmm(target, use_local);
            if fetching {
                format!("showing {showing_s} · fetching {target_s}…")
            } else {
                format!("showing {showing_s} · no data at {target_s}")
            }
        }
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(15.0),
        color,
    );
}
