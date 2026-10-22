//! 3D globe rendering orchestration.
//!
//! Coordinates the WebGL2 paint callbacks for the globe sphere, geographic
//! line overlays, each site's radar projected onto the sphere surface, and
//! optional 3D volumetric ray-marching, all within a single
//! `egui::PaintCallback`.

use crate::core::volume_plan::{GlobeDraw, GlobeSite};
use crate::geo::{GeoLineRenderer, GlobeRenderer};
use crate::nexrad::{GlobeRadarRenderer, RadarGpuRenderer, VolumeRayRenderer};
use crate::state::AppState;
use eframe::egui::{self, Rect};
use glow::HasContext;
use std::sync::{Arc, Mutex};

/// One site's renderers and position, cloned into the paint callback.
struct SitePaint {
    lat: f64,
    lon: f64,
    radar: Option<Arc<Mutex<RadarGpuRenderer>>>,
    globe_radar: Option<Arc<Mutex<GlobeRadarRenderer>>>,
    volume_ray: Option<Arc<Mutex<VolumeRayRenderer>>>,
}

impl SitePaint {
    fn of(site: &crate::subsystem::SiteState) -> Self {
        Self {
            lat: site.lat,
            lon: site.lon,
            radar: site.gpu.radar.clone(),
            globe_radar: site.gpu.globe_radar.clone(),
            volume_ray: site.gpu.volume_ray.clone(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_globe(
    ui: &mut egui::Ui,
    rect: &Rect,
    state: &AppState,
    sites: &crate::subsystem::Sites,
    globe_renderer: Option<&Arc<Mutex<GlobeRenderer>>>,
    geo_line_renderer: Option<&Arc<Mutex<GeoLineRenderer>>>,
) {
    let Some(globe_r) = globe_renderer.cloned() else {
        return;
    };
    let geo_r = geo_line_renderer.cloned();
    let volume_enabled = state.viz_state.volume_3d_enabled;
    let camera_pos = state.viz_state.camera.camera_world_pos();
    let plan = crate::core::volume_plan::globe_draw_plan(
        &sites.iter().map(globe_site).collect::<Vec<_>>(),
        volume_enabled,
        [camera_pos.x, camera_pos.y, camera_pos.z],
    );
    let paints: Vec<SitePaint> = sites.iter().map(SitePaint::of).collect();

    let camera = state.viz_state.camera.clone();
    let processing = state.render_processing.clone();
    let volume_density_cutoff = state.viz_state.volume_density_cutoff;
    let geo_vis = crate::geo::geo_line_renderer::VisibleLayers {
        states: state.layer_state.geo.states,
        counties: state.layer_state.geo.counties,
        highways: state.layer_state.geo.highways,
        lakes: state.layer_state.geo.lakes,
    };

    let callback = egui::PaintCallback {
        rect: *rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
            let gl = painter.gl();

            // 1. Draw globe sphere (sets up depth buffer)
            if let Ok(gr) = globe_r.lock() {
                gr.paint(gl, &camera);
            }

            // 2. Draw geo lines on sphere surface
            if let Some(ref glr) = geo_r {
                if let Ok(lr) = glr.lock() {
                    lr.paint(gl, &camera, &geo_vis);
                }
            }

            // 3. Each site's patch or volume, in the plan's order.
            for step in &plan {
                match *step {
                    GlobeDraw::Surface(i) => paint_surface(gl, &paints[i], &camera, &processing),
                    GlobeDraw::Volume(i) => {
                        paint_volume(gl, &paints[i], &camera, &processing, volume_density_cutoff)
                    }
                }
            }

            // Restore GL state for egui
            unsafe {
                gl.disable(glow::DEPTH_TEST);
                gl.disable(glow::CULL_FACE);
                gl.depth_mask(false);
            }
        })),
    };

    ui.painter().add(callback);
}

/// Drape one site's sweep on the sphere from its own flat textures.
fn paint_surface(
    gl: &glow::Context,
    site: &SitePaint,
    camera: &crate::geo::Camera,
    processing: &crate::core::RenderProcessing,
) {
    if let (Some(ref rr), Some(ref grr)) = (&site.radar, &site.globe_radar) {
        if let (Ok(flat_r), Ok(mut globe_r)) = (rr.lock(), grr.lock()) {
            if flat_r.has_data() {
                // Rebuild mesh if site changed
                globe_r.update_site(gl, site.lat, site.lon, flat_r.max_range_km());
                globe_r.paint(gl, camera, &flat_r, processing);
            }
        }
    }
}

/// Ray-march one site's volume, coloured by its own flat renderer's table.
fn paint_volume(
    gl: &glow::Context,
    site: &SitePaint,
    camera: &crate::geo::Camera,
    processing: &crate::core::RenderProcessing,
    density_cutoff: f32,
) {
    if let (Some(ref vr), Some(ref rr)) = (&site.volume_ray, &site.radar) {
        if let (Ok(mut v), Ok(flat_r)) = (vr.lock(), rr.lock()) {
            // Current viewport dimensions, for FBO sizing.
            let mut vp = [0i32; 4];
            unsafe { gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp) };
            v.paint(
                gl,
                camera,
                flat_r.lut_texture(),
                processing,
                flat_r.value_min(),
                flat_r.value_range(),
                density_cutoff,
                vp[2],
                vp[3],
            );
        }
    }
}

/// What the draw plan needs to know about one site: its frame time and
/// position, and what its renderers hold.
fn globe_site(site: &crate::subsystem::SiteState) -> GlobeSite {
    let held = |r: Option<bool>| r.unwrap_or(false);
    GlobeSite {
        frame_secs: site
            .displayed
            .as_ref()
            .map(|d| (d.start_time + d.end_time) / 2.0),
        position: crate::core::volume_plan::site_unit_position(site.lat, site.lon),
        surface_ready: held(
            site.gpu
                .radar
                .as_ref()
                .and_then(|r| r.lock().ok().map(|r| r.has_data())),
        ),
        volume_ready: held(
            site.gpu
                .volume_ray
                .as_ref()
                .and_then(|r| r.lock().ok().map(|r| r.has_data())),
        ),
    }
}
