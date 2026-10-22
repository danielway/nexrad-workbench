//! Central canvas UI: radar visualization area.

use super::canvas_data_probe::{render_data_probe, render_distance_measurement};
use super::canvas_interaction::{handle_canvas_interaction, handle_globe_interaction};
use super::canvas_overlays::{
    draw_globe, draw_national_mosaic, layout_chrome, render_alerts, render_chrome_overlays,
    render_gps_location, render_mping_detail, render_mping_reports, render_nexrad_sites,
    render_radar_sweep, viewport_bounds, AlertRenderPhase, OverlayContext, RadarCutout,
    SiteLabelRoom,
};
use crate::core::geo_labels::{GeoLabelPlacementKey, GeoLabelVisibilityKey};
use crate::core::RenderProcessing;
use crate::geo::{GeoLayerSet, MapProjection, ScreenBounds, ViewMode};
use crate::nexrad::RadarGpuRenderer;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, Rect, Sense, Stroke};
use geo_types::Coord;
use std::sync::{Arc, Mutex};

/// Render canvas with optional geographic layers and NEXRAD data.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_canvas_with_geo(
    root_ui: &mut egui::Ui,
    state: &mut AppState,
    sites: &crate::subsystem::Sites,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    geo_layers: Option<&GeoLayerSet>,
    gpu: &crate::GpuResources,
) {
    let globe_renderer = gpu.globe.as_ref();
    let geo_line_renderer = gpu.geo_line.as_ref();
    let ctx = root_ui.ctx().clone();
    // The single-valued overlays (rings, probe, cells, captions) describe the
    // focus; the radar discs are drawn for every site in `Sites::drawn`.
    let site = sites.focus();
    let gpu_renderer = site.gpu.radar.as_ref();
    // Every site on the map, in list order: each is drawn highlighted with
    // its id, and none is a pick target.
    let on_map: Vec<&str> = sites.iter().map(|s| s.id.0.as_str()).collect();
    egui::CentralPanel::default().show(root_ui, |ui| {
        let available_size = ui.available_size();

        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        let dark = state.is_dark;

        // Draw background
        painter.rect_filled(rect, 0.0, theme::MAP);

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

                // Draw the 3D globe via PaintCallback. The 3D views draw the
                // focus site only; every site joins them in PR 5 of the
                // multi-site series.
                draw_globe(ui, &rect, state, site, globe_renderer, geo_line_renderer);

                // Corner-chrome overlays (color scale + info + compass)
                // dispatched through the typed registry. The trait's
                // `visible` predicate keeps the scale bar (2D-only) out.
                let bottom_scrims = {
                    let overlay_ctx = OverlayContext {
                        rect,
                        state,
                        site,
                        live,
                        derived,
                        mping_reports: &diagnostics.mping.reports,
                        playback_secs: playback.state.playback_position(),
                    };
                    let chrome_frame = layout_chrome(ui, &overlay_ctx, None);
                    render_chrome_overlays(ui, &overlay_ctx, &chrome_frame)
                };

                // The caption is the same chrome in 3D (§7).
                if state.is_mobile {
                    render_phone_loading(ui, &rect, chrome.phone_loading.as_ref());
                } else {
                    render_canvas_caption(ui, &rect, state, live, &bottom_scrims);
                }

                // Handle orbit/zoom interactions
                handle_globe_interaction(&response, &rect, state);
            }
            ViewMode::Flat2D => {
                // --- Existing flat 2D path ---
                // On the phone the map holds still while the chrome slides
                // (core::phone::hold_map_still): shift the pan by the
                // opposite of the canvas center's move.
                if state.is_mobile {
                    let viewport = ctx.input(|i| i.viewport_rect()).size();
                    let now = crate::core::phone::CanvasFrame {
                        center: (rect.center().x, rect.center().y),
                        viewport: (viewport.x, viewport.y),
                    };
                    let (dx, dy) = crate::core::phone::hold_map_still(chrome.last_canvas, now);
                    chrome.last_canvas = Some(now);
                    if dx != 0.0 || dy != 0.0 {
                        let pan = state.viz_state.pan_offset();
                        state.viz_state.set_pan_offset(pan + egui::vec2(dx, dy));
                    }
                }
                let (centre_lat, centre_lon) = state.viz_state.map_centre;
                let mut projection = MapProjection::new(centre_lat, centre_lon);
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
                // label placement rebuilds on time even when nothing else is
                // animating. No-op if already settled.
                if let Some(remaining) =
                    state.render_cache.camera_motion.time_until_settle(now_secs)
                {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(
                            (remaining * 1000.0).ceil().max(1.0) as u64,
                        ));
                }

                // Every circle around the site from one core decision (§7
                // "the rings agree"): range rings and the edge ring at the
                // data's real range — the same range the GPU disc and the 3D
                // drape use — and the coverage ring and mosaic cutout at the
                // product's maximum range.
                let data_range_km = data_range_km(gpu_renderer);
                let site_screen =
                    super::canvas_overlays::SiteScreen::new(&projection, site.lat, site.lon);
                let coverage_km =
                    crate::core::canvas::product_max_range_km(state.viz_state.product);
                let radar_cutout = data_range_km.map(|_| RadarCutout {
                    center: site_screen.center,
                    radius: coverage_km as f32 * site_screen.px_per_km,
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
                    );
                }

                render_nexrad_sites(
                    &painter,
                    &projection,
                    &on_map,
                    &state.layer_state.geo,
                    crate::geo::GeoPass::Lines,
                    None,
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

                let sweep_info = compute_sweep_line_azimuth(site, playback);
                let (gpu_sweep, between_sweeps, live_partial) =
                    compute_gpu_sweep_state(state, site, playback, derived, sweep_info);

                // Age desaturation needs the estimated antenna azimuth; only
                // supply it while the view's sweep animation is active so a
                // fixed elevation filter doesn't keep fading between visits.
                let chunk_boundary = (derived.effective_sweep_animation
                    && derived.canvas_follows_live)
                    .then_some(site.live.radar_model.estimated_azimuth)
                    .flatten();

                // Live compositing keeps `gpu_sweep` set even when animation
                // is off; force desaturation off in that case so age fades
                // only accompany an active sweep animation (#134).
                let mut processing = state.render_processing.clone();
                processing.data_age_desaturation =
                    crate::core::canvas::data_age_desaturation_effective(
                        processing.data_age_desaturation,
                        derived.effective_sweep_animation,
                    );
                // Every other site's own reveal, so its disc and its Rays
                // marks follow its own antenna. The between-sweeps hold is
                // the focus's alone.
                let others: Vec<OtherReveal<'_>> = sites
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != sites.focus)
                    .map(|(_, s)| {
                        let follows_live = live.canvas_follows_live(&s.live, &playback.state);
                        let (sweep, _) = site_gpu_sweep(
                            state,
                            s,
                            playback,
                            follows_live,
                            derived.effective_sweep_animation,
                            compute_sweep_line_azimuth(s, playback),
                        );
                        OtherReveal {
                            site: s,
                            follows_live,
                            sweep,
                            chunk_boundary: (derived.effective_sweep_animation && follows_live)
                                .then_some(s.live.radar_model.estimated_azimuth)
                                .flatten(),
                        }
                    })
                    .collect();

                // Every site's disc, older frame first so the newer one is
                // on top (core::sites::draw_order), each with its own reveal.
                for drawn in sites.drawn() {
                    let (sweep, boundary) = if std::ptr::eq(drawn, site) {
                        (gpu_sweep, chunk_boundary)
                    } else {
                        others
                            .iter()
                            .find(|o| std::ptr::eq(o.site, drawn))
                            .map_or((None, None), |o| (o.sweep, o.chunk_boundary))
                    };
                    if let Some(renderer) = drawn.gpu.radar.as_ref() {
                        draw_radar_gpu(
                            ui,
                            &projection,
                            renderer,
                            &rect,
                            drawn.lat,
                            drawn.lon,
                            &processing,
                            sweep,
                            boundary,
                        );
                    }
                }

                if gpu_renderer.is_some() {
                    // Boundary ring at the edge of the active site's data range,
                    // matching the mosaic cutout so the seam reads as a single circle.
                    if let Some(c) = radar_cutout {
                        painter.circle_stroke(
                            c.center,
                            c.radius,
                            Stroke::new(1.5_f32, theme::map::ring_major()),
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
                    let live_has_active_sweep = site
                        .live
                        .radar_model
                        .active_sweep
                        .as_ref()
                        .is_some_and(|s| s.data_azimuth_range.is_some());
                    let live_has_moving_line = site.live.radar_model.active
                        && site.live.radar_model.estimated_azimuth.is_some();

                    // A paused archive reveal is a still image; only a moving
                    // playhead or the live stream needs the smooth cadence.
                    let reveal_moving = (gpu_sweep.is_some() || between_sweeps)
                        && (playback.state.playing || site.live.radar_model.active);
                    if reveal_moving || live_has_active_sweep {
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

                // The map's chrome is laid out before the labels paint, so
                // the labels can keep clear of it (core::map_chrome); it
                // paints over everything further down.
                let chrome_frame = layout_chrome(
                    ui,
                    &OverlayContext {
                        rect,
                        state,
                        site,
                        live,
                        derived,
                        mping_reports: &diagnostics.mping.reports,
                        playback_secs: playback.state.playback_position(),
                    },
                    ui.ctx()
                        .memory(|m| m.area_rect(egui::Id::new(MAP_TOOLBAR_ID)))
                        .map(|r| r.size()),
                );
                let keep_out = chrome_frame.keep_out();
                let viewport = viewport_bounds(rect);
                let mut map_labels = Vec::new();

                // Labels pass: draw on top of radar + alerts so text stays
                // legible over bright reflectivity fills. Halo-stroked inside
                // the renderer for contrast.
                if let Some(layers) = geo_layers {
                    let visibility = &state.layer_state.geo;
                    let placement_key = GeoLabelPlacementKey {
                        projection: projection.fingerprint(),
                        visibility: GeoLabelVisibilityKey::from(visibility),
                        dark,
                    };

                    if state
                        .render_cache
                        .geo_labels
                        .needs_rebuild(placement_key, camera_settled)
                    {
                        let candidates = crate::geo::build_geo_label_candidates(
                            &painter,
                            layers,
                            visibility,
                            &projection,
                            state.viz_state.zoom(),
                            dark,
                        );
                        state.render_cache.geo_labels.replace(
                            placement_key,
                            &candidates,
                            ScreenBounds {
                                min_x: rect.min.x,
                                min_y: rect.min.y,
                                max_x: rect.max.x,
                                max_y: rect.max.y,
                            },
                        );
                    }

                    map_labels = crate::geo::paint_geo_labels(
                        &painter,
                        &projection,
                        visibility,
                        &state.render_cache.geo_labels.entries,
                        &|b| crate::core::map_chrome::label_clear_of_chrome(b, &keep_out, viewport),
                    );
                }

                render_nexrad_sites(
                    &painter,
                    &projection,
                    &on_map,
                    &state.layer_state.geo,
                    crate::geo::GeoPass::Labels,
                    Some(&SiteLabelRoom {
                        map_labels: &map_labels,
                        keep_out: &keep_out,
                        viewport,
                    }),
                );

                // Storm cells: what draws is the core's view-model (on, 2D,
                // behind the Rays sweep line; provisional on a live partial).
                // The desktop pointer reaches it as an intent, in lat/lon.
                if !state.is_mobile {
                    let hover = response
                        .hover_pos()
                        .filter(|_| state.viz_state.storm_cells_visible)
                        .map(|p| {
                            let g = projection.screen_to_geo(p);
                            (g.y, g.x)
                        });
                    if hover != state.viz_state.storm_cell_pointer.hover {
                        state.push_command(crate::core::Intent::StormCells(
                            crate::core::storm_cell_view::StormCellIntent::Hover(hover),
                        ));
                        ui.ctx().request_repaint();
                    }
                }
                // Tracks on these cells, with closest approach to "My
                // location" when that layer has a fix.
                let my_location = state
                    .layer_state
                    .geo
                    .gps_location
                    .then_some(diagnostics.gps.coords)
                    .flatten();
                let storm_tracks_view = state
                    .viz_state
                    .storm_tracks_visible
                    .then(|| state.viz_state.storm_tracks.view(my_location))
                    .flatten();
                // Storm cells are the focus site's only; the other discs
                // gain theirs in PR 6 of the multi-site series.
                let storm_cell_inputs = crate::core::storm_cell_view::StormCellViewInputs {
                    result: &site.cells.result,
                    visible: state.viz_state.storm_cells_visible,
                    view_mode: ViewMode::Flat2D,
                    radar: (site.lat, site.lon),
                    site_id: &site.id.0,
                    sweep: gpu_sweep,
                    provisional: live_partial,
                    phone: state.is_mobile,
                    pointer: &state.viz_state.storm_cell_pointer,
                    probe_on: state.viz_state.data_probe_enabled,
                    tracks: storm_tracks_view.as_ref(),
                };
                let storm_cells_vm =
                    crate::core::storm_cell_view::build(&storm_cell_inputs, |lat, lon| {
                        let p = projection.geo_to_screen(Coord { x: lon, y: lat });
                        [p.x, p.y]
                    });
                super::canvas_storm_cells::paint_storm_cells(&painter, &storm_cells_vm);

                if state.layer_state.geo.gps_location {
                    if let Some(coords) = diagnostics.gps.coords {
                        render_gps_location(&painter, &projection, coords);
                    }
                }

                // Rays' marks. Live: the data edge comes from received
                // chunks and the now line from the estimated antenna
                // position; archive: the reveal follows the playhead, and
                // between sweeps the last position is held, faint.
                let (sweep_line_info, sweep_stale) = if derived.canvas_follows_live {
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
                // The other discs' marks first, fainter, so the focus's
                // marks stay on top where discs overlap.
                for other in &others {
                    let vm = site_overlay_vm(
                        state,
                        playback,
                        derived,
                        other.site,
                        other.follows_live,
                        data_range_km_of(other.site),
                        (other.sweep, false),
                    );
                    super::canvas_overlays::render_site_sweep_marks(
                        &painter,
                        &vm,
                        &super::canvas_overlays::SiteScreen::new(
                            &projection,
                            other.site.lat,
                            other.site.lon,
                        ),
                        crate::core::canvas::sweep_marks_opacity(false),
                    );
                }
                let overlay_vm = site_overlay_vm(
                    state,
                    playback,
                    derived,
                    site,
                    derived.canvas_follows_live,
                    data_range_km,
                    (sweep_line_info, sweep_stale),
                );
                let clock = crate::ui::time_format::ZoneClock {
                    use_local: state.use_local_time,
                };
                render_radar_sweep(
                    &painter,
                    &overlay_vm,
                    &super::canvas_overlays::SweepPaint {
                        site: &site_screen,
                        rect,
                        hover: response
                            .hover_pos()
                            .filter(|_| !state.viz_state.data_probe_enabled),
                        desaturating: crate::core::canvas::data_age_desaturation_effective(
                            state.render_processing.data_age_desaturation,
                            derived.effective_sweep_animation,
                        ),
                        now_secs: derived.frame_now_secs,
                        clock: &clock,
                    },
                );

                // The distance tool: what draws (snap hint, ends, letters,
                // readout) is the core's view-model.
                let snap_km = crate::core::measure::snap_radius_km(
                    state.is_mobile,
                    super::canvas_overlays::compute_km_per_pixel(&projection, &rect).unwrap_or(0.0),
                );
                let measure_hover = response.hover_pos().map(|p| {
                    let g = projection.screen_to_geo(p);
                    (g.y, g.x)
                });
                let measure_vm =
                    crate::core::measure::view(&crate::core::measure::MeasureViewInputs {
                        measurement: &state.viz_state.measurement,
                        tool_active: state.viz_state.distance_tool_active,
                        targets: crate::core::measure::SnapTargets {
                            tracks: storm_tracks_view
                                .as_ref()
                                .filter(|_| state.viz_state.storm_cells_visible),
                            my_location,
                        },
                        hover: measure_hover,
                        snap_km,
                    });
                render_distance_measurement(&painter, &projection, &measure_vm, &|r| {
                    crate::core::map_chrome::label_clear_of_chrome(
                        super::canvas_overlays::viewport_bounds(r),
                        &keep_out,
                        viewport,
                    )
                });

                if state.viz_state.data_probe_enabled {
                    if let Some(hover_pos) = response.hover_pos() {
                        let geo = projection.screen_to_geo(hover_pos);
                        let cell_line = crate::core::storm_cell_view::probe_line(
                            &storm_cell_inputs,
                            geo.y,
                            geo.x,
                        );
                        let site_lines = (on_map.len() > 1)
                            .then(|| probe_site_lines(sites, geo.y, geo.x, state, gpu_sweep));
                        render_data_probe(
                            &painter,
                            &projection,
                            hover_pos,
                            site.lat,
                            site.lon,
                            gpu_renderer,
                            &state.viz_state.product,
                            state.use_local_time,
                            gpu_sweep,
                            cell_line,
                            site_lines,
                        );
                    }
                }

                // Hover states (§7): every clickable mark gets a highlight and
                // a pointing cursor, previewing exactly what a click picks.
                if let Some((lat, lon)) = measure_hover {
                    if crate::core::measure::can_grab(
                        &state.viz_state.measurement,
                        state.viz_state.distance_tool_active,
                        lat,
                        lon,
                        snap_km,
                    ) {
                        ui.ctx().set_cursor_icon(
                            if state.viz_state.measurement.dragging.is_some() {
                                egui::CursorIcon::Grabbing
                            } else {
                                egui::CursorIcon::Grab
                            },
                        );
                    }
                }
                if let Some(pos) = response.hover_pos() {
                    if !state.viz_state.distance_tool_active && !response.dragged() {
                        if let Some(pick) = super::canvas_interaction::pick_at(
                            pos,
                            &projection,
                            state,
                            site,
                            &on_map,
                            playback,
                            diagnostics,
                            derived,
                            &storm_cells_vm,
                        ) {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            paint_pick_hover(&painter, &projection, &pick, pos, rect, diagnostics);
                        } else if on_map.len() > 1 && !state.viz_state.data_probe_enabled {
                            // A radar on the map reads its frame's time and age.
                            if let Some(label) =
                                radar_hover_label(sites, &projection, pos, state, &clock)
                            {
                                paint_hover_label(&painter, label, pos, rect);
                            }
                        }
                    }
                }

                if state.layer_state.geo.mping && diagnostics.mping.selected_report_id.is_some() {
                    render_mping_detail(
                        &painter,
                        rect,
                        &projection,
                        &diagnostics.mping.reports,
                        diagnostics.mping.selected_report_id,
                        site.lat,
                        site.lon,
                        playback.state.playback_position(),
                        state.use_local_time,
                    );
                }

                // Corner-chrome overlays (color scale + info + scale
                // bar) dispatched through the typed registry. The
                // trait's `visible` predicate keeps the compass
                // (3D-only) out.
                let bottom_scrims = render_chrome_overlays(
                    ui,
                    &OverlayContext {
                        rect,
                        state,
                        site,
                        live,
                        derived,
                        mping_reports: &diagnostics.mping.reports,
                        playback_secs: playback.state.playback_position(),
                    },
                    &chrome_frame,
                );

                if state.is_mobile {
                    render_phone_loading(ui, &rect, chrome.phone_loading.as_ref());
                } else {
                    render_canvas_caption(ui, &rect, state, live, &bottom_scrims);
                }
                if let Some(card) = storm_cells_vm.card.as_ref() {
                    super::canvas_storm_cells::paint_card(
                        ui.ctx(),
                        rect,
                        card,
                        state.is_mobile,
                        &bottom_scrims,
                    );
                }
                render_map_toolbar(ui, &rect, state);

                // Mobile chrome reveal tap (spec §13): when the chrome was
                // hidden, the resolver latched this frame's press as a reveal.
                // Swallow it here so the same tap doesn't also pan/zoom the
                // map — the chrome is already coming back this frame.
                // Likewise a pinch the phone strip owns zooms the strip,
                // not the map.
                if !chrome.mobile_auto_hide.revealed_this_frame && !chrome.phone_strip_claims_touch
                {
                    handle_canvas_interaction(
                        &response,
                        &rect,
                        state,
                        site,
                        &on_map,
                        playback,
                        diagnostics,
                        derived,
                        &projection,
                        &storm_cells_vm,
                    );
                }
            }
        }
    });
}

/// The map toolbar's area id; its last-frame size feeds the chrome layout.
const MAP_TOOLBAR_ID: &str = "map_toolbar";

/// The map toolbar (one-instrument §8): the data probe and the distance
/// tool as two buttons in the map's corner, where the cursor is. A click is
/// [`crate::core::Intent::ToggleMapTool`]; the active tool is the selected
/// color.
fn render_map_toolbar(ui: &egui::Ui, rect: &egui::Rect, state: &mut AppState) {
    use crate::core::panels::MapTool;
    use egui_phosphor::regular as icons;
    let tools = [
        (
            MapTool::Probe,
            icons::CROSSHAIR,
            state.viz_state.data_probe_enabled,
            "Data probe: hover the radar for the value under the cursor",
        ),
        (
            MapTool::Distance,
            icons::RULER,
            state.viz_state.distance_tool_active,
            "Measure: click two points for the distance between them. A point on a \
             lettered storm follows it; drag an end to move it",
        ),
    ];
    let inset = crate::core::map_chrome::TOOLBAR_INSET;
    egui::Area::new(egui::Id::new(MAP_TOOLBAR_ID))
        .fixed_pos(rect.left_top() + egui::vec2(inset, inset))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme::map::scrim())
                .stroke(egui::Stroke::new(1.0, theme::map::scrim_edge()))
                .corner_radius(4.0)
                .inner_margin(2.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (tool, glyph, on, hover) in tools {
                            let color = if on { theme::SELECTED } else { theme::TEXT_DIM };
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(glyph).size(theme::BODY).color(color),
                                    )
                                    .frame(on),
                                )
                                .on_hover_text(hover)
                                .clicked()
                            {
                                state.push_command(crate::core::Intent::ToggleMapTool(tool));
                            }
                        }
                    });
                });
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

/// Another site's reveal this frame: what its disc and its Rays marks draw.
struct OtherReveal<'a> {
    site: &'a crate::subsystem::SiteState,
    follows_live: bool,
    sweep: Option<(f32, f32)>,
    chunk_boundary: Option<f32>,
}

/// The range a site's disc holds data to, when it holds any.
fn data_range_km(renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>) -> Option<f64> {
    renderer.and_then(|renderer| {
        let r = renderer.lock().expect("renderer mutex poisoned");
        r.has_data().then(|| r.max_range_km())
    })
}

fn data_range_km_of(site: &crate::subsystem::SiteState) -> Option<f64> {
    data_range_km(site.gpu.radar.as_ref())
}

/// The overlay view-model for one site's disc. `sweep` is the reveal and
/// whether it is the held position between sweeps.
fn site_overlay_vm(
    state: &AppState,
    playback: &crate::subsystem::Playback,
    derived: &crate::subsystem::Derived,
    site: &crate::subsystem::SiteState,
    follows_live: bool,
    data_range_km: Option<f64>,
    sweep: (Option<(f32, f32)>, bool),
) -> crate::core::canvas::CanvasOverlayVm {
    let active_sweep = site.live.radar_model.active_sweep.as_ref();
    let chunk_last_az: Vec<f32> = active_sweep
        .map(|s| s.chunks.iter().map(|c| c.last_az).collect())
        .unwrap_or_default();
    crate::core::canvas::CanvasOverlayVm::build(&crate::core::canvas::CanvasOverlayInputs {
        product: state.viz_state.product,
        data_range_km,
        sweep: sweep.0,
        stale: sweep.1,
        show_lines: derived.effective_sweep_animation,
        live: follows_live,
        antenna_az: site.live.radar_model.estimated_azimuth,
        chunk_last_az: &chunk_last_az,
        live_edge_time: active_sweep
            .and_then(|s| s.chunk_time_spans.last().map(|&(_, end, _)| end)),
        offscreen_cut: crate::core::canvas::offscreen_live_cut(
            playback.state.playback_mode() == crate::core::PlaybackMode::Macro,
            site.live
                .radar_model
                .selected_cut(&state.viz_state.elevation_selection),
            site.live.radar_model.collecting_elevation(),
            active_sweep.and_then(|s| s.data_azimuth_range),
        ),
        playhead_secs: playback.state.playback_position(),
        fallbacks: crate::core::canvas::CANVAS_FALLBACKS,
    })
}

/// The focus's reveal: [`site_gpu_sweep`], plus the between-sweeps hold
/// that keeps the last position on screen. Returns the reveal, whether it is
/// held between sweeps, and whether a live partial composites.
fn compute_gpu_sweep_state(
    state: &mut AppState,
    site: &crate::subsystem::SiteState,
    playback: &crate::subsystem::Playback,
    derived: &crate::subsystem::Derived,
    sweep_info: Option<(f32, f32)>,
) -> (Option<(f32, f32)>, bool, bool) {
    let (gpu_sweep, live_partial) = site_gpu_sweep(
        state,
        site,
        playback,
        derived.canvas_follows_live,
        derived.effective_sweep_animation,
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
    (gpu_sweep, between_sweeps, live_partial)
}

/// One site's sweep reveal this frame, and whether it is a live partial
/// still arriving. `follows_live` is whether the site's canvas follows its
/// stream; `animate` is whether Rays animates.
fn site_gpu_sweep(
    state: &AppState,
    site: &crate::subsystem::SiteState,
    playback: &crate::subsystem::Playback,
    follows_live: bool,
    animate: bool,
    sweep_info: Option<(f32, f32)>,
) -> (Option<(f32, f32)>, bool) {
    // Live partial compositing only when the in-progress elevation matches
    // the user's filter (Latest accepts any cut). Otherwise other tilts would
    // keep driving the GPU sweep boundary and age desaturation between visits
    // of a fixed elevation. Frames mode never composites the partial.
    // Archive animation still uses effective_sweep_animation.
    let selected_elev = site
        .live
        .radar_model
        .selected_cut(&state.viz_state.elevation_selection);
    let live_range = crate::core::canvas::live_compositing_azimuth_range(
        follows_live,
        playback.state.playback_mode() == crate::core::PlaybackMode::Macro,
        selected_elev,
        site.live.radar_model.collecting_elevation(),
        site.live
            .radar_model
            .active_sweep
            .as_ref()
            .and_then(|s| s.data_azimuth_range),
    );
    let playback_ts = playback.state.playback_position();
    let sweep_bounds = if live_range.is_none() && animate {
        site.scans
            .find_recent_scan(playback_ts, crate::core::recency::RECENCY_SECS)
            .and_then(|scan| {
                let displayed_elev = site.displayed.as_ref().map(|d| d.identity.elevation_number);
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
        animate,
        sweep_bounds,
        playback_ts,
        sweep_info,
    );

    // A live partial composites only while `live_range` is set: the sweep on
    // the GPU is still arriving.
    (gpu_sweep, live_range.is_some())
}

fn compute_sweep_line_azimuth(
    site: &crate::subsystem::SiteState,
    playback: &crate::subsystem::Playback,
) -> Option<(f32, f32)> {
    if playback.state.speed.timeline_seconds_per_real_second() > 30.0 {
        return None;
    }

    let ts = playback.state.playback_position();

    // Find the sweep the playhead is in, then interpolate its azimuth (pure).
    site.scans
        .find_scan_at_timestamp(ts)
        .and_then(|scan| scan.find_sweep_at_timestamp(ts))
        .and_then(|(_, sweep)| crate::core::canvas::sweep_line_azimuth(sweep, ts))
}

pub(super) fn format_unix_timestamp(ts: f64, use_local: bool) -> String {
    let p = super::time_format::parts(ts, use_local);
    let zone = if use_local { "Local" } else { "UTC" };
    format!(
        "{:02}:{:02}:{:02}.{:03} {zone}",
        p.hour, p.minute, p.second, p.millis
    )
}

/// The phone's loading state on the map (phone timeline v2): while the
/// frame under the playhead downloads, the frame on screen stays visible
/// under a 40 % dim, with a small pill in the middle naming what is coming
/// (`Loading 23:49`) beside the row's arc. The phone draws no caption: the
/// row and this say it.
fn render_phone_loading(
    ui: &egui::Ui,
    rect: &Rect,
    loading: Option<&crate::core::phone_row::LoadingVm>,
) {
    let Some(l) = loading else {
        return;
    };
    let painter = ui
        .ctx()
        .layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("phone_loading"),
        ))
        .with_clip_rect(*rect);
    painter.rect_filled(*rect, 0.0, theme::phone::loading_scrim());
    let galley = painter.layout_no_wrap(
        l.label.clone(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
    );
    let size = egui::vec2(10.0 + 18.0 + 10.0 + galley.size().x + 14.0, 36.0);
    let pill = Rect::from_center_size(rect.center(), size);
    painter.rect(
        pill,
        size.y / 2.0,
        theme::alpha(theme::PANEL, 218),
        Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    let reduced = super::timeline::reduced_motion(ui.ctx());
    super::timeline::mini::paint_arc(
        &painter,
        egui::pos2(pill.left() + 10.0 + 9.0, pill.center().y),
        7.0,
        l.arc,
        ui.ctx().input(|i| i.time),
        reduced,
    );
    let text_at = egui::pos2(pill.left() + 38.0, pill.center().y - galley.size().y / 2.0);
    painter.galley(text_at, galley, theme::TEXT);
}

/// Paint the canvas caption (§7) on a scrim at the bottom center of the map,
/// lifted over any corner scrim it would cover
/// ([`crate::core::canvas::caption_bottom_inset`]) — the only text that ever
/// sits over it. The words and their roles are the
/// session view-model's ([`crate::core::session::SessionVm::canvas_caption_line`]);
/// a "fetch" segment emits the line's intent.
fn render_canvas_caption(
    ui: &mut egui::Ui,
    rect: &Rect,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    bottom_scrims: &[crate::core::canvas::BottomScrim],
) {
    use crate::core::canvas::CaptionRole;
    let Some(line) = live
        .session
        .canvas_caption_line(&crate::ui::time_format::ZoneClock {
            use_local: state.use_local_time,
        })
    else {
        return;
    };
    let font = theme::readout_font();
    let id = egui::Id::new("canvas_caption");
    // The caption's width is known once laid out: last frame's.
    let width = ui
        .ctx()
        .memory(|m| m.area_rect(id))
        .map_or(0.0, |r| r.width());
    let inset = crate::core::canvas::caption_bottom_inset(rect.width(), width, bottom_scrims);
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(rect.center().x, rect.bottom() - inset))
        .interactable(line.action.is_some())
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme::map::scrim())
                .stroke(Stroke::new(1.0, theme::map::scrim_edge()))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    // A phone-width map wraps the line rather than cutting
                    // it at both edges.
                    ui.set_max_width((rect.width() - 40.0).max(120.0));
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal_wrapped(|ui| {
                        for (words, role) in &line.segments {
                            let color = match role {
                                CaptionRole::Words => theme::TEXT_DIM,
                                CaptionRole::Frame => theme::SEEN,
                                CaptionRole::Playhead { pinned: true } => theme::NOW,
                                CaptionRole::Playhead { pinned: false } => theme::SEEN,
                                CaptionRole::Action => theme::SELECTED,
                            };
                            let text = egui::RichText::new(words).font(font.clone()).color(color);
                            if *role == CaptionRole::Action {
                                let r = ui
                                    .add(egui::Label::new(text.underline()).sense(Sense::click()));
                                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    if let Some(intent) = line.action.clone() {
                                        state.push_command(intent);
                                    }
                                }
                            } else {
                                ui.label(text);
                            }
                        }
                    });
                });
        });
}

/// Paint a hovered mark's highlight and its label (on a scrim beside the
/// cursor). What is hovered is the core's [`crate::core::canvas::MapPick`].
fn paint_pick_hover(
    painter: &egui::Painter,
    projection: &MapProjection,
    pick: &crate::core::canvas::MapPick,
    cursor: egui::Pos2,
    rect: Rect,
    diagnostics: &crate::subsystem::Diagnostics,
) {
    use crate::core::canvas::MapPick;
    let mark = match pick {
        MapPick::Site { lat, lon, .. } => Some((*lat, *lon)),
        MapPick::Mping(id) => diagnostics
            .mping
            .reports
            .iter()
            .find(|r| r.id == *id)
            .map(|r| (r.lat, r.lon)),
        MapPick::Alert { .. } | MapPick::StormCell { .. } => None,
    };
    if let Some((lat, lon)) = mark {
        let at = projection.geo_to_screen(Coord { x: lon, y: lat });
        painter.circle_stroke(at, 9.0, Stroke::new(1.5_f32, theme::SEEN));
    }
    let Some(label) = pick.hover_label() else {
        return;
    };
    paint_hover_label(painter, label, cursor, rect);
}

/// A hover label on a scrim beside the cursor, kept inside the canvas.
fn paint_hover_label(painter: &egui::Painter, label: String, cursor: egui::Pos2, rect: Rect) {
    let galley = painter.layout_no_wrap(label, theme::readout_font(), theme::TEXT);
    let size = galley.size() + egui::vec2(10.0, 6.0);
    let mut min = cursor + egui::vec2(12.0, 12.0);
    min.x = min.x.min(rect.max.x - size.x);
    min.y = min.y.min(rect.max.y - size.y);
    let r = Rect::from_min_size(min, size);
    painter.rect(
        r,
        4.0,
        theme::map::scrim(),
        Stroke::new(1.0_f32, theme::map::scrim_edge()),
        egui::StrokeKind::Inside,
    );
    painter.galley(r.min + egui::vec2(5.0, 3.0), galley, theme::TEXT);
}

/// The label of the radar on the map nearest the cursor, within the site
/// pick radius ([`crate::core::site_canvas::radar_hover_label`]).
fn radar_hover_label(
    sites: &crate::subsystem::Sites,
    projection: &MapProjection,
    cursor: egui::Pos2,
    state: &AppState,
    clock: &dyn crate::core::session::Clock,
) -> Option<String> {
    let radius = crate::core::canvas::pick_radii(state.is_mobile).site;
    sites
        .iter()
        .map(|s| {
            let at = projection.geo_to_screen(Coord { x: s.lon, y: s.lat });
            (s, (at - cursor).length())
        })
        .filter(|&(_, d)| d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| {
            let frame_time = s
                .displayed
                .as_ref()
                .map(|d| (d.start_time + d.end_time) / 2.0);
            crate::core::site_canvas::radar_hover_label(
                &s.id.0,
                frame_time,
                s.data_staleness_secs,
                clock,
            )
        })
}

/// One probe line per site whose disc is under the cursor, focus first
/// ([`crate::core::site_canvas::probe_order`]).
fn probe_site_lines(
    sites: &crate::subsystem::Sites,
    lat: f64,
    lon: f64,
    state: &AppState,
    sweep: Option<(f32, f32)>,
) -> Vec<String> {
    use crate::core::site_canvas::{probe_order, probe_site_line, ProbeReading};
    let readings: Vec<ProbeReading> = sites
        .iter()
        .map(|s| {
            let (azimuth_deg, range_km) = crate::core::canvas::geo_to_polar(lat, lon, s.lat, s.lon);
            let (max_range_km, value) = s
                .gpu
                .radar
                .as_ref()
                .map(|r| {
                    let r = r.lock().expect("renderer mutex poisoned");
                    (
                        r.has_data().then(|| r.max_range_km()),
                        r.value_at_polar(azimuth_deg as f32, range_km, sweep),
                    )
                })
                .unwrap_or((None, None));
            ProbeReading {
                id: s.id.0.clone(),
                azimuth_deg,
                range_km,
                max_range_km,
                value,
            }
        })
        .collect();
    let product = &state.viz_state.product;
    probe_order(&readings, sites.focus)
        .into_iter()
        .map(|i| probe_site_line(&readings[i], product.short_code(), product.unit()))
        .collect()
}
