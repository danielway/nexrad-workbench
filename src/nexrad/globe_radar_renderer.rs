//! Globe-mode radar renderer.
//!
//! Renders radar data on a spherical cap mesh for 3D globe display.
//! The fragment shader reuses the same data lookup, interpolation, and
//! color LUT logic as the flat renderer — only the coordinate source differs:
//! polar coords come from vertex attributes instead of screen-space math.

use crate::geo::camera::GlobeCamera;
use crate::state::RenderProcessing;
use glow::HasContext;
use std::sync::Arc;

/// Radius of the radar patch above the unit sphere (avoid z-fighting).
const PATCH_RADIUS: f32 = 1.003;

/// Number of azimuth steps in the radar patch mesh.
const PATCH_AZ_STEPS: u32 = 180;
/// Number of range steps in the radar patch mesh.
const PATCH_RANGE_STEPS: u32 = 60;

// ── Globe-mode shaders ─────────────────────────────────────────────

const GLOBE_VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;

uniform mat4 u_view_projection;

in vec3 a_position;  // 3D point on sphere
in vec2 a_polar;     // (azimuth_deg, range_km)

out vec2 v_polar;

void main() {
    v_polar = a_polar;
    gl_Position = u_view_projection * vec4(a_position, 1.0);
}
"#;

/// Build the globe-mode fragment shader from shared GLSL snippets.
///
/// Nearly identical to the flat-mode fragment shader, but receives
/// (azimuth_deg, range_km) as interpolated vertex attributes
/// instead of computing them from screen-space pixel position.
fn build_globe_fragment_shader() -> String {
    use super::shader_common::*;
    format!(
        r#"#version 300 es
precision highp float;

in vec2 v_polar; // (azimuth_deg, range_km)
out vec4 fragColor;

{FRAGMENT_PREAMBLE}
{SAMPLE_DATA_P}
{IS_VALID}
{FIND_NEAREST_AZ_P}
{FIND_BRACKET_AZ_P}
void main() {{
    float azimuth_deg = v_polar.x;
    float dist_km = v_polar.y;

    if (dist_km < u_first_gate_km || dist_km >= u_max_range_km) {{
        discard;
    }}

    float gate_idx = (dist_km - u_first_gate_km) / u_gate_interval_km;

    if (gate_idx < 0.0 || gate_idx >= u_gate_count) {{
        discard;
    }}

    float s_gate_count = u_gate_count;
    float s_azimuth_count = u_azimuth_count;
    float s_offset = u_offset;
    float s_scale = u_scale;

    float value;

    if (u_interpolation == 1) {{
        float az_lo, az_hi, az_frac;
        if (!find_bracket_az_p(azimuth_deg, u_azimuth_count, u_azimuth_tex, az_lo, az_hi, az_frac)) {{
            discard;
        }}

        float g_lo = floor(gate_idx);
        float g_hi = min(g_lo + 1.0, u_gate_count - 1.0);
        float g_frac = gate_idx - g_lo;

        float v00 = sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, g_lo, az_lo);
        float v10 = sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, g_hi, az_lo);
        float v01 = sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, g_lo, az_hi);
        float v11 = sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, g_hi, az_hi);

        float sum = 0.0;
        float wsum = 0.0;
        float w00 = (1.0 - g_frac) * (1.0 - az_frac);
        float w10 = g_frac * (1.0 - az_frac);
        float w01 = (1.0 - g_frac) * az_frac;
        float w11 = g_frac * az_frac;

        if (is_valid(v00)) {{ sum += v00 * w00; wsum += w00; }}
        if (is_valid(v10)) {{ sum += v10 * w10; wsum += w10; }}
        if (is_valid(v01)) {{ sum += v01 * w01; wsum += w01; }}
        if (is_valid(v11)) {{ sum += v11 * w11; wsum += w11; }}

        if (wsum < 0.001) {{
            discard;
        }}
        value = sum / wsum;
    }} else {{
        float dummy_az;
        float best_idx = find_nearest_az_p(azimuth_deg, u_azimuth_count, u_azimuth_tex, dummy_az);
        if (best_idx < 0.0) {{
            discard;
        }}
        value = sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, floor(gate_idx), best_idx);
    }}

    if (!is_valid(value)) {{
        discard;
    }}

    // Despeckle
    if (u_despeckle_enabled == 1) {{
        float dummy_az2;
        float center_az = find_nearest_az_p(azimuth_deg, u_azimuth_count, u_azimuth_tex, dummy_az2);
        float center_g = floor(gate_idx);
        if (center_az >= 0.0) {{
            int valid_count = 0;
            for (int dg = -1; dg <= 1; dg++) {{
                for (int da = -1; da <= 1; da++) {{
                    if (dg == 0 && da == 0) continue;
                    float ng = center_g + float(dg);
                    float na = mod(center_az + float(da), u_azimuth_count);
                    if (is_valid(sample_data_p(u_data_tex, u_gate_count, u_azimuth_count, ng, na))) {{
                        valid_count++;
                    }}
                }}
            }}
            if (valid_count < u_despeckle_threshold) {{
                discard;
            }}
        }}
    }}

{RAW_TO_PHYSICAL}
{COLOR_LOOKUP}
{PREMULTIPLIED_ALPHA_OUTPUT}
}}
"#
    )
}

/// Globe-mode radar renderer — renders radar data on a spherical cap mesh.
pub struct GlobeRadarRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    _vbo: glow::Buffer,
    ibo: glow::Buffer,
    index_count: i32,

    // Uniform locations
    u_view_projection: glow::UniformLocation,
    u_gate_count: glow::UniformLocation,
    u_azimuth_count: glow::UniformLocation,
    u_first_gate_km: glow::UniformLocation,
    u_gate_interval_km: glow::UniformLocation,
    u_max_range_km: glow::UniformLocation,
    u_value_min: glow::UniformLocation,
    u_value_range: glow::UniformLocation,
    u_interpolation: glow::UniformLocation,
    u_despeckle_enabled: glow::UniformLocation,
    u_despeckle_threshold: glow::UniformLocation,
    u_opacity: glow::UniformLocation,
    u_offset: glow::UniformLocation,
    u_scale: glow::UniformLocation,

    /// Radar site location (for rebuilding mesh when site changes).
    site_lat: f64,
    site_lon: f64,
    /// Max range in km that the current mesh covers.
    mesh_range_km: f64,
}

impl GlobeRadarRenderer {
    pub fn new(gl: &Arc<glow::Context>) -> Self {
        unsafe { Self::new_inner(gl) }
    }

    unsafe fn new_inner(gl: &Arc<glow::Context>) -> Self {
        let globe_frag = build_globe_fragment_shader();
        let program =
            crate::geo::globe_renderer::compile_program(gl, GLOBE_VERTEX_SHADER, &globe_frag);

        // Get uniform locations
        let u_view_projection = gl
            .get_uniform_location(program, "u_view_projection")
            .unwrap();
        let u_gate_count = gl.get_uniform_location(program, "u_gate_count").unwrap();
        let u_azimuth_count = gl.get_uniform_location(program, "u_azimuth_count").unwrap();
        let u_first_gate_km = gl.get_uniform_location(program, "u_first_gate_km").unwrap();
        let u_gate_interval_km = gl
            .get_uniform_location(program, "u_gate_interval_km")
            .unwrap();
        let u_max_range_km = gl.get_uniform_location(program, "u_max_range_km").unwrap();
        let u_value_min = gl.get_uniform_location(program, "u_value_min").unwrap();
        let u_value_range = gl.get_uniform_location(program, "u_value_range").unwrap();
        let u_interpolation = gl.get_uniform_location(program, "u_interpolation").unwrap();
        let u_despeckle_enabled = gl
            .get_uniform_location(program, "u_despeckle_enabled")
            .unwrap();
        let u_despeckle_threshold = gl
            .get_uniform_location(program, "u_despeckle_threshold")
            .unwrap();
        let u_opacity = gl.get_uniform_location(program, "u_opacity").unwrap();
        let u_offset = gl.get_uniform_location(program, "u_offset").unwrap();
        let u_scale = gl.get_uniform_location(program, "u_scale").unwrap();

        // Bind texture samplers
        gl.use_program(Some(program));
        if let Some(loc) = gl.get_uniform_location(program, "u_data_tex") {
            gl.uniform_1_i32(Some(&loc), 0);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_lut_tex") {
            gl.uniform_1_i32(Some(&loc), 1);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_azimuth_tex") {
            gl.uniform_1_i32(Some(&loc), 2);
        }
        gl.use_program(None);

        // Create empty mesh (will be rebuilt when site is set)
        let vao = gl.create_vertex_array().unwrap();
        let vbo = gl.create_buffer().unwrap();
        let ibo = gl.create_buffer().unwrap();

        // Set up vertex attributes
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

        let stride = 5 * 4; // 3 floats position + 2 floats polar = 5 * 4 bytes
        let a_position = gl.get_attrib_location(program, "a_position").unwrap();
        gl.enable_vertex_attrib_array(a_position);
        gl.vertex_attrib_pointer_f32(a_position, 3, glow::FLOAT, false, stride, 0);

        let a_polar = gl.get_attrib_location(program, "a_polar").unwrap();
        gl.enable_vertex_attrib_array(a_polar);
        gl.vertex_attrib_pointer_f32(a_polar, 2, glow::FLOAT, false, stride, 3 * 4);

        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
        gl.bind_vertex_array(None);

        Self {
            program,
            vao,
            _vbo: vbo,
            ibo,
            index_count: 0,
            u_view_projection,
            u_gate_count,
            u_azimuth_count,
            u_first_gate_km,
            u_gate_interval_km,
            u_max_range_km,
            u_value_min,
            u_value_range,
            u_interpolation,
            u_despeckle_enabled,
            u_despeckle_threshold,
            u_opacity,
            u_offset,
            u_scale,
            site_lat: 0.0,
            site_lon: 0.0,
            mesh_range_km: 0.0,
        }
    }

    /// Rebuild the spherical cap mesh for a radar site.
    /// Call when the radar site changes or on first data load.
    pub fn update_site(&mut self, gl: &glow::Context, lat: f64, lon: f64, max_range_km: f64) {
        if (self.site_lat - lat).abs() < 1e-6
            && (self.site_lon - lon).abs() < 1e-6
            && (self.mesh_range_km - max_range_km).abs() < 1.0
        {
            return; // mesh already up to date
        }
        self.site_lat = lat;
        self.site_lon = lon;
        self.mesh_range_km = max_range_km;

        let (verts, indices) = generate_radar_patch(lat, lon, max_range_km);
        self.index_count = indices.len() as i32;

        unsafe {
            gl.bind_vertex_array(Some(self.vao));

            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self._vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                cast_f32_to_u8(&verts),
                glow::STATIC_DRAW,
            );

            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ibo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                cast_u32_to_u8(&indices),
                glow::STATIC_DRAW,
            );

            gl.bind_vertex_array(None);
        }
    }

    /// Paint radar data on the globe. Textures must already be bound by the
    /// flat renderer (units 0=data, 1=lut, 2=azimuth).
    pub fn paint(
        &self,
        gl: &glow::Context,
        camera: &GlobeCamera,
        flat_renderer: &super::gpu_renderer::RadarGpuRenderer,
        processing: &RenderProcessing,
    ) {
        if self.index_count == 0 || !flat_renderer.has_data() {
            return;
        }

        unsafe {
            // Depth test already enabled by globe renderer, no depth write
            gl.depth_mask(false);

            // Premultiplied alpha blending
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );

            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ibo));

            // Bind the flat renderer's textures (they're already uploaded)
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(flat_renderer.data_texture()));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(flat_renderer.lut_texture()));
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(flat_renderer.azimuth_texture()));

            // View-projection matrix
            let vp = camera.view_projection();
            gl.uniform_matrix_4_f32_slice(
                Some(&self.u_view_projection),
                false,
                &vp.to_cols_array(),
            );

            // Data metadata (from the flat renderer)
            gl.uniform_1_f32(Some(&self.u_gate_count), flat_renderer.gate_count() as f32);
            gl.uniform_1_f32(
                Some(&self.u_azimuth_count),
                flat_renderer.azimuth_count() as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_first_gate_km),
                flat_renderer.first_gate_km() as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_gate_interval_km),
                flat_renderer.gate_interval_km() as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_max_range_km),
                flat_renderer.max_range_km() as f32,
            );
            gl.uniform_1_f32(Some(&self.u_value_min), flat_renderer.value_min());
            gl.uniform_1_f32(Some(&self.u_value_range), flat_renderer.value_range());

            // Processing uniforms
            let interp_mode = match processing.interpolation {
                crate::state::InterpolationMode::Nearest => 0,
                crate::state::InterpolationMode::Bilinear => 1,
            };
            gl.uniform_1_i32(Some(&self.u_interpolation), interp_mode);
            gl.uniform_1_i32(
                Some(&self.u_despeckle_enabled),
                processing.despeckle_enabled as i32,
            );
            gl.uniform_1_i32(
                Some(&self.u_despeckle_threshold),
                processing.despeckle_threshold as i32,
            );
            gl.uniform_1_f32(Some(&self.u_opacity), processing.opacity);

            // Raw-to-physical conversion
            gl.uniform_1_f32(Some(&self.u_offset), flat_renderer.data_offset());
            gl.uniform_1_f32(Some(&self.u_scale), flat_renderer.data_scale());

            gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
            gl.depth_mask(true);
        }
    }
}

/// Generate a spherical cap mesh centered on (lat, lon) covering max_range_km.
///
/// Each vertex has 5 floats: [x, y, z, azimuth_deg, range_km].
/// Uses great-circle math to compute the position on the sphere for each
/// (azimuth, range) sample point.
fn generate_radar_patch(
    center_lat_deg: f64,
    center_lon_deg: f64,
    max_range_km: f64,
) -> (Vec<f32>, Vec<u32>) {
    let earth_radius_km = 6371.0;
    let az_steps = PATCH_AZ_STEPS;
    let range_steps = PATCH_RANGE_STEPS;

    let clat = center_lat_deg.to_radians();
    let clon = center_lon_deg.to_radians();

    let mut verts = Vec::new();
    let mut indices = Vec::new();

    // Generate vertices: (range_steps + 1) rings × (az_steps) sectors + center
    // Center vertex
    let cx = PATCH_RADIUS * (clat.cos() * clon.sin()) as f32;
    let cy = PATCH_RADIUS * clat.sin() as f32;
    let cz = PATCH_RADIUS * (clat.cos() * clon.cos()) as f32;
    verts.extend_from_slice(&[cx, cy, cz, 0.0, 0.0]); // center: az=0, range=0

    // Ring vertices
    for ri in 1..=range_steps {
        let range_km = max_range_km * ri as f64 / range_steps as f64;
        let angular_dist = range_km / earth_radius_km; // radians on sphere

        for ai in 0..az_steps {
            let azimuth_deg = 360.0 * ai as f64 / az_steps as f64;
            let az_rad = azimuth_deg.to_radians();

            // Great-circle destination from (clat, clon) along bearing az_rad for angular_dist
            let lat2 = (clat.sin() * angular_dist.cos()
                + clat.cos() * angular_dist.sin() * az_rad.cos())
            .asin();
            let lon2 = clon
                + (az_rad.sin() * angular_dist.sin() * clat.cos())
                    .atan2(angular_dist.cos() - clat.sin() * lat2.sin());

            let x = PATCH_RADIUS * (lat2.cos() * lon2.sin()) as f32;
            let y = PATCH_RADIUS * lat2.sin() as f32;
            let z = PATCH_RADIUS * (lat2.cos() * lon2.cos()) as f32;

            verts.extend_from_slice(&[x, y, z, azimuth_deg as f32, range_km as f32]);
        }
    }

    // Indices: center fan for first ring
    for ai in 0..az_steps {
        let next = (ai + 1) % az_steps;
        indices.push(0); // center
        indices.push(1 + ai);
        indices.push(1 + next);
    }

    // Indices: quads between successive rings
    for ri in 1..range_steps {
        let ring0_start = 1 + (ri - 1) * az_steps;
        let ring1_start = 1 + ri * az_steps;
        for ai in 0..az_steps {
            let next = (ai + 1) % az_steps;
            let v00 = ring0_start + ai;
            let v01 = ring0_start + next;
            let v10 = ring1_start + ai;
            let v11 = ring1_start + next;

            indices.push(v00);
            indices.push(v10);
            indices.push(v01);

            indices.push(v01);
            indices.push(v10);
            indices.push(v11);
        }
    }

    (verts, indices)
}

fn cast_f32_to_u8(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

fn cast_u32_to_u8(data: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}
