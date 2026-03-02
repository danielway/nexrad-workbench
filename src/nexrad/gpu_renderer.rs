//! GPU-based radar renderer using WebGL2 shaders via glow.
//!
//! Renders polar radar data (azimuths x gates) directly on the GPU using a fragment
//! shader that performs polar-to-Cartesian conversion and color lookup from a LUT texture.

use glow::HasContext;
use nexrad_render::Product;
use std::sync::Arc;

/// Sentinel value used to mark no-data gates in the data texture.
#[allow(dead_code)]
const SENTINEL: f32 = -9999.0;

// Default value ranges per product (used for color LUT normalization).
fn product_value_range(product: Product) -> (f32, f32) {
    match product {
        Product::Reflectivity => (-32.0, 95.0),
        Product::Velocity => (-64.0, 64.0),
        Product::SpectrumWidth => (0.0, 30.0),
        Product::DifferentialReflectivity => (-2.0, 6.0),
        Product::CorrelationCoefficient => (0.0, 1.05),
        Product::DifferentialPhase => (0.0, 360.0),
        Product::ClutterFilterPower => (-20.0, 20.0),
    }
}

fn product_from_str(s: &str) -> Product {
    match s {
        "velocity" => Product::Velocity,
        "spectrum_width" => Product::SpectrumWidth,
        "differential_reflectivity" => Product::DifferentialReflectivity,
        "differential_phase" => Product::DifferentialPhase,
        "correlation_coefficient" => Product::CorrelationCoefficient,
        "clutter_filter_power" => Product::ClutterFilterPower,
        _ => Product::Reflectivity,
    }
}

const VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;

in vec2 a_position;
out vec2 v_screen_pos;

uniform vec2 u_viewport_size;

void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    // Convert NDC (-1..1) to pixel coordinates in egui convention (Y-down).
    // WebGL NDC has Y-up, so flip Y so (0,0) = top-left to match the
    // radar center coordinate passed from egui screen space.
    vec2 uv = a_position * 0.5 + 0.5;
    v_screen_pos = vec2(uv.x, 1.0 - uv.y) * u_viewport_size;
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;

in vec2 v_screen_pos;
out vec4 fragColor;

uniform vec2 u_radar_center;       // radar center in screen pixels
uniform float u_radar_radius;      // max coverage radius in screen pixels
uniform float u_gate_count;
uniform float u_azimuth_count;
uniform float u_first_gate_km;
uniform float u_gate_interval_km;
uniform float u_max_range_km;
uniform float u_value_min;
uniform float u_value_range;

uniform sampler2D u_data_tex;      // gate values (R32F, width=gates, height=azimuths)
uniform sampler2D u_lut_tex;       // color LUT (RGBA8, 256x1)
uniform sampler2D u_azimuth_tex;   // azimuth angles (R32F, Nx1)

const float SENTINEL = -9999.0;
const float PI = 3.14159265359;

void main() {
    // Offset from radar center in pixels
    vec2 delta = v_screen_pos - u_radar_center;

    // Distance in pixels -> km
    float dist_px = length(delta);
    float dist_km = (dist_px / u_radar_radius) * u_max_range_km;

    // Outside radar range or inside first gate
    if (dist_km < u_first_gate_km || dist_km >= u_max_range_km) {
        fragColor = vec4(0.0);
        return;
    }

    // Azimuth angle: 0=North(up), clockwise
    // Screen: +x=right, +y=down. atan(x, -y) gives 0 at north.
    float azimuth_rad = atan(delta.x, -delta.y);
    float azimuth_deg = mod(degrees(azimuth_rad) + 360.0, 360.0);

    // Find nearest radial using estimated index (azimuths ~uniformly spaced)
    float az_spacing = 360.0 / u_azimuth_count;
    float est_idx = azimuth_deg / az_spacing;
    float inv_count = 1.0 / u_azimuth_count;

    float best_idx = 0.0;
    float best_dist = 360.0;

    // Search around estimated index
    for (float offset = -2.0; offset <= 2.0; offset += 1.0) {
        float i = mod(est_idx + offset, u_azimuth_count);
        i = floor(i);
        float tex_az = texture(u_azimuth_tex, vec2((i + 0.5) * inv_count, 0.5)).r;
        float d = abs(azimuth_deg - tex_az);
        d = min(d, 360.0 - d);  // wraparound
        if (d < best_dist) {
            best_dist = d;
            best_idx = i;
        }
    }

    // Gap detection: skip if angular distance > 1.5x spacing
    if (best_dist > az_spacing * 1.5) {
        fragColor = vec4(0.0);
        return;
    }

    // Gate index
    float gate_idx = (dist_km - u_first_gate_km) / u_gate_interval_km;
    if (gate_idx < 0.0 || gate_idx >= u_gate_count) {
        fragColor = vec4(0.0);
        return;
    }

    // Sample data texture (nearest neighbor)
    float gate_u = (floor(gate_idx) + 0.5) / u_gate_count;
    float az_v = (best_idx + 0.5) / u_azimuth_count;
    float value = texture(u_data_tex, vec2(gate_u, az_v)).r;

    // No-data check
    if (value <= SENTINEL + 1.0) {
        fragColor = vec4(0.0);
        return;
    }

    // Normalize and look up color
    float normalized = clamp((value - u_value_min) / u_value_range, 0.0, 1.0);
    vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));

    // Output premultiplied alpha (egui requirement)
    fragColor = vec4(color.rgb * color.a, color.a);
}
"#;

/// GPU-based radar renderer using WebGL2 shaders.
#[allow(dead_code)]
pub struct RadarGpuRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,

    data_texture: glow::Texture,
    lut_texture: glow::Texture,
    azimuth_texture: glow::Texture,

    // Uniform locations
    u_radar_center: glow::UniformLocation,
    u_radar_radius: glow::UniformLocation,
    u_gate_count: glow::UniformLocation,
    u_azimuth_count: glow::UniformLocation,
    u_first_gate_km: glow::UniformLocation,
    u_gate_interval_km: glow::UniformLocation,
    u_max_range_km: glow::UniformLocation,
    u_value_min: glow::UniformLocation,
    u_value_range: glow::UniformLocation,
    u_viewport_size: glow::UniformLocation,

    // Data metadata
    azimuth_count: u32,
    gate_count: u32,
    first_gate_km: f64,
    gate_interval_km: f64,
    max_range_km: f64,
    value_min: f32,
    value_range: f32,
    has_data: bool,
}

impl RadarGpuRenderer {
    /// Create a new GPU renderer, compiling shaders and allocating GL resources.
    pub fn new(gl: &Arc<glow::Context>) -> Self {
        unsafe {
            let program = gl.create_program().expect("Cannot create program");

            let vert = gl.create_shader(glow::VERTEX_SHADER).expect("Cannot create vertex shader");
            gl.shader_source(vert, VERTEX_SHADER);
            gl.compile_shader(vert);
            if !gl.get_shader_compile_status(vert) {
                let info = gl.get_shader_info_log(vert);
                log::error!("Vertex shader compile error: {}", info);
            }

            let frag = gl.create_shader(glow::FRAGMENT_SHADER).expect("Cannot create fragment shader");
            gl.shader_source(frag, FRAGMENT_SHADER);
            gl.compile_shader(frag);
            if !gl.get_shader_compile_status(frag) {
                let info = gl.get_shader_info_log(frag);
                log::error!("Fragment shader compile error: {}", info);
            }

            gl.attach_shader(program, vert);
            gl.attach_shader(program, frag);
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let info = gl.get_program_info_log(program);
                log::error!("Shader program link error: {}", info);
            }
            gl.detach_shader(program, vert);
            gl.detach_shader(program, frag);
            gl.delete_shader(vert);
            gl.delete_shader(frag);

            // Fullscreen quad (two triangles)
            let vertices: [f32; 12] = [
                -1.0, -1.0,
                 1.0, -1.0,
                 1.0,  1.0,
                -1.0, -1.0,
                 1.0,  1.0,
                -1.0,  1.0,
            ];

            let vbo = gl.create_buffer().expect("Cannot create VBO");
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast_slice(&vertices),
                glow::STATIC_DRAW,
            );

            let vao = gl.create_vertex_array().expect("Cannot create VAO");
            gl.bind_vertex_array(Some(vao));
            let a_position = gl.get_attrib_location(program, "a_position").expect("Missing a_position");
            gl.enable_vertex_attrib_array(a_position);
            gl.vertex_attrib_pointer_f32(a_position, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);

            // Create placeholder textures (1x1)
            let data_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let azimuth_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let lut_texture = create_rgba8_texture(gl, 1, 1, &[0, 0, 0, 0]);

            // Bind texture units to sampler uniforms
            gl.use_program(Some(program));

            let u_data_tex = gl.get_uniform_location(program, "u_data_tex").expect("Missing u_data_tex");
            gl.uniform_1_i32(Some(&u_data_tex), 0);
            let u_lut_tex = gl.get_uniform_location(program, "u_lut_tex").expect("Missing u_lut_tex");
            gl.uniform_1_i32(Some(&u_lut_tex), 1);
            let u_azimuth_tex = gl.get_uniform_location(program, "u_azimuth_tex").expect("Missing u_azimuth_tex");
            gl.uniform_1_i32(Some(&u_azimuth_tex), 2);

            let u_radar_center = gl.get_uniform_location(program, "u_radar_center").expect("Missing u_radar_center");
            let u_radar_radius = gl.get_uniform_location(program, "u_radar_radius").expect("Missing u_radar_radius");
            let u_gate_count = gl.get_uniform_location(program, "u_gate_count").expect("Missing u_gate_count");
            let u_azimuth_count = gl.get_uniform_location(program, "u_azimuth_count").expect("Missing u_azimuth_count");
            let u_first_gate_km = gl.get_uniform_location(program, "u_first_gate_km").expect("Missing u_first_gate_km");
            let u_gate_interval_km = gl.get_uniform_location(program, "u_gate_interval_km").expect("Missing u_gate_interval_km");
            let u_max_range_km = gl.get_uniform_location(program, "u_max_range_km").expect("Missing u_max_range_km");
            let u_value_min = gl.get_uniform_location(program, "u_value_min").expect("Missing u_value_min");
            let u_value_range = gl.get_uniform_location(program, "u_value_range").expect("Missing u_value_range");
            let u_viewport_size = gl.get_uniform_location(program, "u_viewport_size").expect("Missing u_viewport_size");

            gl.use_program(None);

            Self {
                program,
                vao,
                vbo,
                data_texture,
                lut_texture,
                azimuth_texture,
                u_radar_center,
                u_radar_radius,
                u_gate_count,
                u_azimuth_count,
                u_first_gate_km,
                u_gate_interval_km,
                u_max_range_km,
                u_value_min,
                u_value_range,
                u_viewport_size,
                azimuth_count: 0,
                gate_count: 0,
                first_gate_km: 0.0,
                gate_interval_km: 0.0,
                max_range_km: 0.0,
                value_min: 0.0,
                value_range: 1.0,
                has_data: false,
            }
        }
    }

    /// Upload decoded radar data to GPU textures.
    ///
    /// `gate_values` should already have sentinel encoding for non-valid gates.
    pub fn update_data(
        &mut self,
        gl: &glow::Context,
        azimuths: &[f32],
        gate_values: &[f32],
        azimuth_count: u32,
        gate_count: u32,
        first_gate_km: f64,
        gate_interval_km: f64,
        max_range_km: f64,
    ) {
        self.azimuth_count = azimuth_count;
        self.gate_count = gate_count;
        self.first_gate_km = first_gate_km;
        self.gate_interval_km = gate_interval_km;
        self.max_range_km = max_range_km;
        self.has_data = azimuth_count > 0 && gate_count > 0;

        if !self.has_data {
            return;
        }

        unsafe {
            // Re-create data texture (gates x azimuths, R32F)
            gl.delete_texture(self.data_texture);
            self.data_texture = create_r32f_texture(
                gl,
                gate_count as i32,
                azimuth_count as i32,
                gate_values,
            );

            // Re-create azimuth texture (Nx1, R32F)
            gl.delete_texture(self.azimuth_texture);
            self.azimuth_texture = create_r32f_texture(
                gl,
                azimuth_count as i32,
                1,
                azimuths,
            );
        }

        log::info!(
            "GPU data uploaded: {}x{} (azimuths x gates), range {:.1}-{:.1} km",
            azimuth_count,
            gate_count,
            first_gate_km,
            max_range_km,
        );
    }

    /// Build and upload a color lookup table for the given product.
    pub fn update_color_table(&mut self, gl: &glow::Context, product_str: &str) {
        let product = product_from_str(product_str);
        let (min_val, max_val) = product_value_range(product);
        self.value_min = min_val;
        self.value_range = max_val - min_val;

        let color_scale = nexrad_render::default_color_scale(product);

        // Build 256-entry RGBA LUT
        let lut_size = 256usize;
        let mut lut_data = Vec::with_capacity(lut_size * 4);
        for i in 0..lut_size {
            let t = i as f32 / (lut_size - 1) as f32;
            let value = min_val + t * (max_val - min_val);
            let color = color_scale.color(value);
            let rgba = color.to_rgba8();
            lut_data.extend_from_slice(&rgba);
        }

        unsafe {
            gl.delete_texture(self.lut_texture);
            self.lut_texture = create_rgba8_texture(gl, lut_size as i32, 1, &lut_data);
        }

        log::info!(
            "GPU LUT uploaded for {:?}: {:.1}..{:.1}",
            product,
            min_val,
            max_val,
        );
    }

    /// Returns true if radar data has been uploaded.
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// Maximum range of the currently loaded data in km.
    pub fn max_range_km(&self) -> f64 {
        self.max_range_km
    }

    /// Clear all radar data (e.g. on site change).
    pub fn clear_data(&mut self) {
        self.has_data = false;
    }

    /// Render the radar data using the current GL context.
    ///
    /// Called from within an `egui_glow::CallbackFn`.
    /// `radar_center` and `radar_radius` are in physical pixels (not points).
    ///
    /// egui_glow restores its own GL state after each paint callback,
    /// so we don't need to save/restore state ourselves.
    pub fn paint(
        &self,
        gl: &glow::Context,
        radar_center: [f32; 2],
        radar_radius: f32,
        viewport_size: [f32; 2],
    ) {
        if !self.has_data {
            return;
        }

        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));

            // Premultiplied alpha blending
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.disable(glow::SCISSOR_TEST);

            // Bind textures to units
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.data_texture));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.lut_texture));
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.azimuth_texture));

            // Set uniforms
            gl.uniform_2_f32(Some(&self.u_radar_center), radar_center[0], radar_center[1]);
            gl.uniform_1_f32(Some(&self.u_radar_radius), radar_radius);
            gl.uniform_1_f32(Some(&self.u_gate_count), self.gate_count as f32);
            gl.uniform_1_f32(Some(&self.u_azimuth_count), self.azimuth_count as f32);
            gl.uniform_1_f32(Some(&self.u_first_gate_km), self.first_gate_km as f32);
            gl.uniform_1_f32(Some(&self.u_gate_interval_km), self.gate_interval_km as f32);
            gl.uniform_1_f32(Some(&self.u_max_range_km), self.max_range_km as f32);
            gl.uniform_1_f32(Some(&self.u_value_min), self.value_min);
            gl.uniform_1_f32(Some(&self.u_value_range), self.value_range);
            gl.uniform_2_f32(Some(&self.u_viewport_size), viewport_size[0], viewport_size[1]);

            // Draw fullscreen quad
            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            // Unbind our resources so we don't interfere with egui
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
        }
    }

    /// Clean up GL resources.
    #[allow(dead_code)]
    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_texture(self.data_texture);
            gl.delete_texture(self.lut_texture);
            gl.delete_texture(self.azimuth_texture);
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Cast an `&[f32]` to `&[u8]` for GL upload.
fn bytemuck_cast_slice(data: &[f32]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
    }
}

/// Create an R32F texture with the given dimensions and data.
unsafe fn create_r32f_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    data: &[f32],
) -> glow::Texture {
    let texture = gl.create_texture().expect("Cannot create texture");
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::R32F as i32,
        width,
        height,
        0,
        glow::RED,
        glow::FLOAT,
        glow::PixelUnpackData::Slice(Some(bytemuck_cast_slice(data))),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    texture
}

/// Create an RGBA8 texture with the given dimensions and data.
unsafe fn create_rgba8_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    data: &[u8],
) -> glow::Texture {
    let texture = gl.create_texture().expect("Cannot create texture");
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        width,
        height,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(data)),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    texture
}
