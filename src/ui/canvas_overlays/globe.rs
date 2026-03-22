//! 3D globe rendering orchestration.
//!
//! Coordinates the WebGL2 paint callbacks for the globe sphere, geographic
//! line overlays, 2D radar projection onto the sphere surface, and optional
//! 3D volumetric ray-marching — all within a single `egui::PaintCallback`.

use crate::geo::{GeoLineRenderer, GlobeRenderer};
use crate::nexrad::RadarGpuRenderer;
use crate::state::AppState;
use eframe::egui::{self, Rect};
use glow::HasContext;
use std::sync::{Arc, Mutex};

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_globe(
    ui: &mut egui::Ui,
    rect: &Rect,
    state: &AppState,
    globe_renderer: Option<&Arc<Mutex<GlobeRenderer>>>,
    geo_line_renderer: Option<&Arc<Mutex<GeoLineRenderer>>>,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
    globe_radar_renderer: Option<&Arc<Mutex<crate::nexrad::GlobeRadarRenderer>>>,
    volume_ray_renderer: Option<&Arc<Mutex<crate::nexrad::VolumeRayRenderer>>>,
) {
    let Some(globe_r) = globe_renderer.cloned() else {
        return;
    };
    let geo_r = geo_line_renderer.cloned();
    let radar_r = gpu_renderer.cloned();
    let globe_rr = globe_radar_renderer.cloned();
    let vol_r = volume_ray_renderer.cloned();

    let camera = state.viz_state.camera.clone();
    let processing = state.render_processing.clone();
    let radar_lat = state.viz_state.center_lat;
    let radar_lon = state.viz_state.center_lon;
    let volume_enabled = state.viz_state.volume_3d_enabled;
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

            // 3. Draw radar data on sphere
            let mut drew_volume = false;
            if volume_enabled {
                if let (Some(ref vr), Some(ref rr)) = (&vol_r, &radar_r) {
                    if let (Ok(mut v), Ok(flat_r)) = (vr.lock(), rr.lock()) {
                        if v.has_data() {
                            // Get current viewport dimensions for FBO sizing
                            let mut vp = [0i32; 4];
                            unsafe { gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp) };
                            v.paint(
                                gl,
                                &camera,
                                flat_r.lut_texture(),
                                &processing,
                                flat_r.value_min(),
                                flat_r.value_range(),
                                volume_density_cutoff,
                                vp[2], // viewport width
                                vp[3], // viewport height
                            );
                            drew_volume = true;
                        }
                    }
                }
            }
            if !drew_volume {
                if let (Some(ref rr), Some(ref grr)) = (&radar_r, &globe_rr) {
                    if let (Ok(flat_r), Ok(mut globe_r)) = (rr.lock(), grr.lock()) {
                        if flat_r.has_data() {
                            // Rebuild mesh if site changed
                            globe_r.update_site(gl, radar_lat, radar_lon, flat_r.max_range_km());
                            // Paint radar on sphere
                            globe_r.paint(gl, &camera, &flat_r, &processing);
                        }
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
