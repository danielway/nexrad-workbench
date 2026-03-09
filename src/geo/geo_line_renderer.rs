//! WebGL2 renderer for geographic line features on the globe surface.
//!
//! Converts `GeoFeature` coordinates to 3D points on a unit sphere
//! (with a slight radius offset to avoid z-fighting with the globe)
//! and draws them as `GL_LINES`.

use crate::geo::camera::GlobeCamera;
use crate::geo::layer::{GeoFeature, GeoLayer, GeoLayerType};
use eframe::egui::Color32;
use glow::HasContext;
use std::sync::Arc;

/// Slight offset above unit sphere for geo lines.
const GEO_LINE_RADIUS: f32 = 1.002;

/// A batch of lines for a single layer type.
struct LayerBatch {
    layer_type: GeoLayerType,
    color: Color32,
    line_width: f32,
    start: i32, // first vertex index
    count: i32, // number of vertices (draw as GL_LINES)
}

pub struct GeoLineRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    _vbo: glow::Buffer,
    batches: Vec<LayerBatch>,
    u_view_projection: glow::UniformLocation,
    u_color: glow::UniformLocation,
}

impl GeoLineRenderer {
    pub fn new(gl: &Arc<glow::Context>) -> Self {
        unsafe { Self::new_inner(gl) }
    }

    unsafe fn new_inner(gl: &Arc<glow::Context>) -> Self {
        let vert_src = r#"#version 300 es
precision highp float;

uniform mat4 u_view_projection;
in vec3 a_position;

void main() {
    gl_Position = u_view_projection * vec4(a_position, 1.0);
}
"#;

        let frag_src = r#"#version 300 es
precision highp float;

uniform vec4 u_color;
out vec4 fragColor;

void main() {
    fragColor = u_color;
}
"#;

        let program = super::globe_renderer::compile_program(gl, vert_src, frag_src);
        let u_view_projection = gl
            .get_uniform_location(program, "u_view_projection")
            .unwrap();
        let u_color = gl.get_uniform_location(program, "u_color").unwrap();

        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));

        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

        let a_position = gl.get_attrib_location(program, "a_position").unwrap();
        gl.enable_vertex_attrib_array(a_position);
        gl.vertex_attrib_pointer_f32(a_position, 3, glow::FLOAT, false, 3 * 4, 0);

        gl.bind_vertex_array(None);

        Self {
            program,
            vao,
            _vbo: vbo,
            batches: Vec::new(),
            u_view_projection,
            u_color,
        }
    }

    /// Upload geo layer geometry. Call once when layers are loaded.
    pub fn upload_layers(&mut self, gl: &glow::Context, layers: &[GeoLayer]) {
        let mut all_verts: Vec<f32> = Vec::new();
        let mut batches: Vec<LayerBatch> = Vec::new();

        for layer in layers {
            // Skip point-only layers (cities) — labels handled by egui
            if layer.layer_type == GeoLayerType::Cities {
                continue;
            }

            let start = (all_verts.len() / 3) as i32;
            for feature in &layer.features {
                emit_feature_lines(feature, &mut all_verts);
            }
            let end = (all_verts.len() / 3) as i32;
            let count = end - start;
            if count > 0 {
                batches.push(LayerBatch {
                    layer_type: layer.layer_type,
                    color: layer.effective_color(),
                    line_width: layer.effective_line_width(),
                    start,
                    count,
                });
            }
        }

        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self._vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                cast_f32_to_u8(&all_verts),
                glow::STATIC_DRAW,
            );
            gl.bind_vertex_array(None);
        }

        self.batches = batches;
    }

    /// Draw geo lines. Expects depth test already enabled by globe renderer.
    pub fn paint(&self, gl: &glow::Context, camera: &GlobeCamera, visible_layers: &VisibleLayers) {
        if self.batches.is_empty() {
            return;
        }

        unsafe {
            // Depth test but no depth write (don't obscure radar)
            gl.depth_mask(false);

            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));

            let vp = camera.view_projection();
            gl.uniform_matrix_4_f32_slice(
                Some(&self.u_view_projection),
                false,
                &vp.to_cols_array(),
            );

            for batch in &self.batches {
                if !visible_layers.is_visible(batch.layer_type) {
                    continue;
                }

                let c = batch.color;
                gl.uniform_4_f32(
                    Some(&self.u_color),
                    c.r() as f32 / 255.0,
                    c.g() as f32 / 255.0,
                    c.b() as f32 / 255.0,
                    c.a() as f32 / 255.0,
                );

                gl.line_width(batch.line_width);
                gl.draw_arrays(glow::LINES, batch.start, batch.count);
            }

            gl.bind_vertex_array(None);
            gl.depth_mask(true);
        }
    }
}

/// Which layer types are currently visible — mirrors the UI toggle state.
pub struct VisibleLayers {
    pub states: bool,
    pub counties: bool,
    pub highways: bool,
    pub lakes: bool,
}

impl VisibleLayers {
    fn is_visible(&self, lt: GeoLayerType) -> bool {
        match lt {
            GeoLayerType::States => self.states,
            GeoLayerType::Counties => self.counties,
            GeoLayerType::Highways => self.highways,
            GeoLayerType::Lakes => self.lakes,
            GeoLayerType::Cities => false, // handled by egui text
        }
    }
}

/// Convert a lat/lon to a 3D point on a sphere of the given radius.
fn geo_to_sphere(lat: f64, lon: f64, radius: f32) -> [f32; 3] {
    let lat_r = (lat as f32).to_radians();
    let lon_r = (lon as f32).to_radians();
    [
        radius * lat_r.cos() * lon_r.sin(),
        radius * lat_r.sin(),
        radius * lat_r.cos() * lon_r.cos(),
    ]
}

/// Emit GL_LINES vertices for a feature (pairs of endpoints).
fn emit_feature_lines(feature: &GeoFeature, verts: &mut Vec<f32>) {
    match feature {
        GeoFeature::LineString(coords) => {
            emit_linestring(coords, verts);
        }
        GeoFeature::MultiLineString(lines) => {
            for line in lines {
                emit_linestring(line, verts);
            }
        }
        GeoFeature::Polygon { exterior, .. } => {
            emit_linestring(exterior, verts);
        }
        GeoFeature::MultiPolygon { polygons, .. } => {
            for (ext, _) in polygons {
                emit_linestring(ext, verts);
            }
        }
        GeoFeature::Point(..) => {} // handled by egui text
    }
}

/// Emit pairs of vertices for GL_LINES from a coordinate ring.
fn emit_linestring(coords: &[geo_types::Coord<f64>], verts: &mut Vec<f32>) {
    for window in coords.windows(2) {
        let a = geo_to_sphere(window[0].y, window[0].x, GEO_LINE_RADIUS);
        let b = geo_to_sphere(window[1].y, window[1].x, GEO_LINE_RADIUS);
        verts.extend_from_slice(&a);
        verts.extend_from_slice(&b);
    }
}

fn cast_f32_to_u8(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}
