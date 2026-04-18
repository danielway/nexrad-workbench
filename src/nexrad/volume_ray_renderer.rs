//! Volumetric ray-march renderer for 3D radar data.
//!
//! Renders all elevation sweeps simultaneously as a semi-transparent volume.
//! A full-screen quad is drawn; the fragment shader fires a ray through each pixel,
//! steps through the radar volume shell, and samples radar data via trilinear
//! interpolation across azimuth, range, and elevation dimensions.

use crate::geo::camera::GlobeCamera;
use crate::nexrad::VolumeSweepMeta;
use crate::state::RenderProcessing;
use glow::HasContext;
use std::sync::Arc;

/// Maximum number of elevation sweeps the shader supports.
const MAX_SWEEPS: usize = 25;

/// Resolution divisor for the offscreen FBO (2 = half-res = 4x fewer pixels).
const RESOLUTION_DIVISOR: u32 = 2;

// ── Shaders ─────────────────────────────────────────────────────────────

const VOLUME_VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;

in vec2 a_position;
out vec2 v_uv;

void main() {
    v_uv = a_position * 0.5 + 0.5;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
"#;

/// Simple blit shader — draws an RGBA texture onto a full-screen quad.
const BLIT_VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 a_position;
out vec2 v_uv;
void main() {
    v_uv = a_position * 0.5 + 0.5;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
"#;

const BLIT_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
out vec4 fragColor;
uniform sampler2D u_tex;
void main() {
    fragColor = texture(u_tex, v_uv);
}
"#;

const VOLUME_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
precision highp int;
precision highp usampler2D;

in vec2 v_uv;
out vec4 fragColor;

// Camera
uniform mat4 u_inv_view_projection;
uniform vec3 u_camera_pos;

// Radar site on unit sphere
uniform vec3 u_site_pos;
uniform float u_site_lat_rad;
uniform float u_site_lon_rad;

// Volume bounds
uniform float u_inner_radius;   // ~1.003
uniform float u_outer_radius;   // ~1.003 + max_beam_height/6371
uniform float u_max_range_km;

// Data texture (packed values in R8UI or R16UI 2D texture)
uniform usampler2D u_volume_tex;
uniform int u_tex_width;
uniform sampler2D u_lut_tex;

// Per-sweep metadata arrays
uniform int u_sweep_count;
uniform float u_elevation[25];
uniform float u_azimuth_count[25];
uniform float u_gate_count[25];
uniform float u_first_gate_km[25];
uniform float u_gate_interval_km[25];
uniform int u_data_offset[25];
uniform float u_scale[25];
uniform float u_offset[25];

// Rendering params
uniform float u_opacity;
uniform float u_density_cutoff;
uniform float u_value_min;
uniform float u_value_range;

const float EARTH_RADIUS = 6371.0;

// Tuning constants
const int MAX_STEPS = 96;
const float ALPHA_CUTOFF = 0.95;

// ── Ray-sphere intersection ──────────────────────────────────────────

vec2 sphere_intersect(vec3 ro, vec3 rd, float radius) {
    float b = dot(ro, rd);
    float c = dot(ro, ro) - radius * radius;
    float disc = b * b - c;
    if (disc < 0.0) return vec2(1e20, -1e20);
    float sq = sqrt(disc);
    return vec2(-b - sq, -b + sq);
}

// ── Nearest-neighbor sweep sample (1 fetch instead of 4) ────────────

float sample_sweep_nn(int si, float az_deg, float range_km) {
    float gc = u_gate_count[si];
    float ac = u_azimuth_count[si];

    float gate_f = (range_km - u_first_gate_km[si]) / u_gate_interval_km[si];
    if (gate_f < 0.0 || gate_f >= gc) return -1.0;

    int gate = int(gate_f);
    int az = int(mod(az_deg * ac / 360.0, ac));

    int idx = u_data_offset[si] + az * int(gc) + gate;
    int y = idx / u_tex_width;
    int x = idx - y * u_tex_width;
    float raw = float(texelFetch(u_volume_tex, ivec2(x, y), 0).r);

    return (raw > 1.5) ? raw : -1.0;
}

// ── Linear search for elevation bracket (faster than binary for ≤25) ─

int find_sweep_bracket(float el_deg) {
    if (u_sweep_count < 2) return -1;
    if (el_deg < u_elevation[0] || el_deg > u_elevation[u_sweep_count - 1]) return -1;

    for (int i = 0; i < 24; i++) {
        if (i + 1 >= u_sweep_count) return -1;
        if (u_elevation[i + 1] >= el_deg) return i;
    }
    return -1;
}

// ── Main ────────────────────────────────────────────────────────────

void main() {
    // Generate world-space ray from screen UV
    vec2 ndc = v_uv * 2.0 - 1.0;
    vec4 near_h = u_inv_view_projection * vec4(ndc, -1.0, 1.0);
    vec4 far_h  = u_inv_view_projection * vec4(ndc,  1.0, 1.0);
    vec3 near_p = near_h.xyz / near_h.w;
    vec3 far_p  = far_h.xyz / far_h.w;
    vec3 rd = normalize(far_p - near_p);
    vec3 ro = u_camera_pos;

    // Intersect with inner and outer bounding spheres
    vec2 t_inner = sphere_intersect(ro, rd, u_inner_radius);
    vec2 t_outer = sphere_intersect(ro, rd, u_outer_radius);

    // If outer sphere missed entirely, skip
    if (t_outer.x > t_outer.y) discard;

    // Determine march range
    float t_start = max(t_outer.x, 0.0);
    float t_end = t_outer.y;

    // Clip to inner sphere (don't render inside earth)
    if (t_inner.x < t_inner.y && t_inner.x > 0.0) {
        t_end = min(t_end, t_inner.x);
    }
    if (t_start >= t_end) discard;

    // Early discard: test if midpoint of the ray is within radar range
    // This cheaply kills ~90% of pixels on the globe that are far from the site
    vec3 mid_pt = normalize(ro + rd * ((t_start + t_end) * 0.5));
    float cos_mid = dot(mid_pt, u_site_pos);
    float cos_max_angle = cos(u_max_range_km / EARTH_RADIUS);
    if (cos_mid < cos_max_angle * 0.7) discard;  // 0.7 = generous margin

    // Adaptive step size — target ~64-96 steps through the shell
    float march_range = t_end - t_start;
    float step = march_range / float(MAX_STEPS);

    // Precompute site trig (hoisted out of loop)
    float slat = sin(u_site_lat_rad);
    float clat = cos(u_site_lat_rad);
    float slon = sin(u_site_lon_rad);
    float clon = cos(u_site_lon_rad);
    vec3 north = vec3(-slat * slon, clat, -slat * clon);
    vec3 east  = vec3(clon, 0.0, -slon);

    // Precompute scale/offset (same for all sweeps of same product)
    float scale_val = u_scale[0];
    float offset_val = u_offset[0];

    // Front-to-back accumulation
    vec3 accum_color = vec3(0.0);
    float accum_alpha = 0.0;

    float t = t_start + step * 0.5;  // offset by half-step to avoid aliasing
    for (int i = 0; i < MAX_STEPS; i++) {
        if (t >= t_end || accum_alpha >= ALPHA_CUTOFF) break;

        vec3 pos = ro + rd * t;

        // Height above unit sphere surface (in km)
        float r = length(pos);
        float h_km = (r - u_inner_radius) * EARTH_RADIUS;

        // Ground distance from site — use cheap dot product approximation
        // cos(angle) ≈ dot(normalize(pos), site_pos), then ground_dist ≈ acos(cos) * R
        // But acos is expensive. Use: dist² ≈ 2R²(1-cos) for small angles
        vec3 p_surf = pos / r;  // normalize
        float cos_gd = dot(p_surf, u_site_pos);

        // Quick range check using squared chord distance (avoids acos)
        // chord² = 2(1 - cos(θ)), ground_dist² ≈ R²·chord² for small angles
        float chord2 = 2.0 * (1.0 - cos_gd);
        float ground_dist2_km = chord2 * EARTH_RADIUS * EARTH_RADIUS;
        float max_r2 = u_max_range_km * u_max_range_km;

        if (ground_dist2_km > max_r2) {
            t += step;
            continue;
        }

        // Full computation only for in-range samples
        float ground_dist_km = sqrt(ground_dist2_km);
        float range_km = sqrt(ground_dist2_km + h_km * h_km);

        if (range_km < 1.0) {
            t += step;
            continue;
        }

        // Elevation angle
        float el_deg = degrees(atan(h_km, ground_dist_km));

        // Find bracketing sweeps
        int si = find_sweep_bracket(el_deg);
        if (si < 0) {
            t += step;
            continue;
        }

        // Azimuth: bearing from site to point
        vec3 d = p_surf - u_site_pos;
        float az_deg = degrees(atan(dot(d, east), dot(d, north)));
        if (az_deg < 0.0) az_deg += 360.0;

        // Sample two bracketing sweeps (nearest-neighbor: 1 fetch each)
        float v_lo = sample_sweep_nn(si, az_deg, range_km);
        float v_hi = sample_sweep_nn(si + 1, az_deg, range_km);

        // Elevation interpolation
        float raw;
        if (v_lo < 0.0 && v_hi < 0.0) {
            t += step;
            continue;
        } else if (v_lo < 0.0) {
            raw = v_hi;
        } else if (v_hi < 0.0) {
            raw = v_lo;
        } else {
            float el_frac = (el_deg - u_elevation[si]) / max(u_elevation[si + 1] - u_elevation[si], 0.01);
            raw = mix(v_lo, v_hi, el_frac);
        }

        // Convert raw to physical value
        float physical = (scale_val != 0.0) ? (raw - offset_val) / scale_val : raw;

        if (physical >= u_density_cutoff) {
            // Color lookup
            float normalized = clamp((physical - u_value_min) / u_value_range, 0.0, 1.0);
            vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));

            // Density-proportional opacity
            float sample_alpha = color.a * u_opacity * step * 300.0;
            sample_alpha = clamp(sample_alpha, 0.0, 0.8);

            // Front-to-back compositing
            accum_color += (1.0 - accum_alpha) * color.rgb * sample_alpha;
            accum_alpha += (1.0 - accum_alpha) * sample_alpha;
        }

        t += step;
    }

    if (accum_alpha < 0.001) discard;

    fragColor = vec4(accum_color, accum_alpha);
}
"#;

// ── Renderer struct ─────────────────────────────────────────────────

pub struct VolumeRayRenderer {
    program: glow::Program,
    quad_vao: glow::VertexArray,
    _quad_vbo: glow::Buffer,
    volume_texture: Option<glow::Texture>,
    tex_width: i32,

    // Half-res offscreen FBO for cheaper rendering
    blit_program: glow::Program,
    fbo: glow::Framebuffer,
    fbo_color: glow::Texture,
    fbo_width: i32,
    fbo_height: i32,

    // Uniform locations (Option because shader may optimize them away)
    u_inv_view_projection: Option<glow::UniformLocation>,
    u_camera_pos: Option<glow::UniformLocation>,
    u_site_pos: Option<glow::UniformLocation>,
    u_site_lat_rad: Option<glow::UniformLocation>,
    u_site_lon_rad: Option<glow::UniformLocation>,
    u_inner_radius: Option<glow::UniformLocation>,
    u_outer_radius: Option<glow::UniformLocation>,
    u_max_range_km: Option<glow::UniformLocation>,
    u_tex_width: Option<glow::UniformLocation>,
    u_sweep_count: Option<glow::UniformLocation>,
    u_elevation: Option<glow::UniformLocation>,
    u_azimuth_count: Option<glow::UniformLocation>,
    u_gate_count: Option<glow::UniformLocation>,
    u_first_gate_km: Option<glow::UniformLocation>,
    u_gate_interval_km: Option<glow::UniformLocation>,
    u_max_range: Option<glow::UniformLocation>,
    u_data_offset: Option<glow::UniformLocation>,
    u_scale: Option<glow::UniformLocation>,
    u_offset: Option<glow::UniformLocation>,
    u_opacity: Option<glow::UniformLocation>,
    u_density_cutoff: Option<glow::UniformLocation>,
    u_value_min: Option<glow::UniformLocation>,
    u_value_range: Option<glow::UniformLocation>,

    has_data: bool,
    sweep_count: i32,
    site_lat: f64,
    site_lon: f64,
}

impl VolumeRayRenderer {
    pub fn new(gl: &Arc<glow::Context>) -> Self {
        unsafe { Self::new_inner(gl) }
    }

    unsafe fn new_inner(gl: &Arc<glow::Context>) -> Self {
        let program = crate::geo::globe_renderer::compile_program(
            gl,
            VOLUME_VERTEX_SHADER,
            VOLUME_FRAGMENT_SHADER,
        );

        // Check if shader compiled/linked successfully by testing a known uniform
        let get = |name: &str| {
            let loc = gl.get_uniform_location(program, name);
            if loc.is_none() {
                log::warn!(
                    "Volume shader: uniform '{}' not found (shader may have failed to compile)",
                    name
                );
            }
            loc
        };
        let u_inv_view_projection = get("u_inv_view_projection");
        let u_camera_pos = get("u_camera_pos");
        let u_site_pos = get("u_site_pos");
        let u_site_lat_rad = get("u_site_lat_rad");
        let u_site_lon_rad = get("u_site_lon_rad");
        let u_inner_radius = get("u_inner_radius");
        let u_outer_radius = get("u_outer_radius");
        let u_max_range_km = get("u_max_range_km");
        let u_tex_width = get("u_tex_width");
        let u_sweep_count = get("u_sweep_count");
        let u_elevation = get("u_elevation[0]");
        let u_azimuth_count = get("u_azimuth_count[0]");
        let u_gate_count = get("u_gate_count[0]");
        let u_first_gate_km = get("u_first_gate_km[0]");
        let u_gate_interval_km = get("u_gate_interval_km[0]");
        let u_max_range = get("u_max_range[0]");
        let u_data_offset = get("u_data_offset[0]");
        let u_scale = get("u_scale[0]");
        let u_offset = get("u_offset[0]");
        let u_opacity = get("u_opacity");
        let u_density_cutoff = get("u_density_cutoff");
        let u_value_min = get("u_value_min");
        let u_value_range = get("u_value_range");

        // Bind texture samplers
        gl.use_program(Some(program));
        if let Some(loc) = gl.get_uniform_location(program, "u_volume_tex") {
            gl.uniform_1_i32(Some(&loc), 0);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_lut_tex") {
            gl.uniform_1_i32(Some(&loc), 1);
        }
        gl.use_program(None);

        // Compile blit shader
        let blit_program = crate::geo::globe_renderer::compile_program(
            gl,
            BLIT_VERTEX_SHADER,
            BLIT_FRAGMENT_SHADER,
        );
        gl.use_program(Some(blit_program));
        if let Some(loc) = gl.get_uniform_location(blit_program, "u_tex") {
            gl.uniform_1_i32(Some(&loc), 0);
        }
        gl.use_program(None);

        // Create initial FBO (1x1, resized on first paint)
        let fbo = gl.create_framebuffer().unwrap();
        let fbo_color = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(fbo_color));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            1,
            1,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(fbo_color),
            0,
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.bind_texture(glow::TEXTURE_2D, None);

        // Create full-screen quad
        let quad_vao = gl.create_vertex_array().unwrap();
        let quad_vbo = gl.create_buffer().unwrap();

        #[rustfmt::skip]
        let quad_verts: [f32; 12] = [
            -1.0, -1.0,
             1.0, -1.0,
            -1.0,  1.0,
            -1.0,  1.0,
             1.0, -1.0,
             1.0,  1.0,
        ];

        gl.bind_vertex_array(Some(quad_vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad_vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            cast_f32_to_u8(&quad_verts),
            glow::STATIC_DRAW,
        );

        let a_position = gl.get_attrib_location(program, "a_position").unwrap();
        gl.enable_vertex_attrib_array(a_position);
        gl.vertex_attrib_pointer_f32(a_position, 2, glow::FLOAT, false, 8, 0);

        gl.bind_vertex_array(None);

        Self {
            program,
            quad_vao,
            _quad_vbo: quad_vbo,
            volume_texture: None,
            tex_width: 0,
            blit_program,
            fbo,
            fbo_color,
            fbo_width: 1,
            fbo_height: 1,
            u_inv_view_projection,
            u_camera_pos,
            u_site_pos,
            u_site_lat_rad,
            u_site_lon_rad,
            u_inner_radius,
            u_outer_radius,
            u_max_range_km,
            u_tex_width,
            u_sweep_count,
            u_elevation,
            u_azimuth_count,
            u_gate_count,
            u_first_gate_km,
            u_gate_interval_km,
            u_max_range,
            u_data_offset,
            u_scale,
            u_offset,
            u_opacity,
            u_density_cutoff,
            u_value_min,
            u_value_range,
            has_data: false,
            sweep_count: 0,
            site_lat: 0.0,
            site_lon: 0.0,
        }
    }

    /// Upload packed volume data and sweep metadata.
    ///
    /// `word_size` is 1 for u8 data (R8UI) or 2 for u16 data (R16UI).
    pub fn update_volume(
        &mut self,
        gl: &glow::Context,
        buffer: &[u8],
        word_size: u8,
        sweeps: &[VolumeSweepMeta],
        site_lat: f64,
        site_lon: f64,
    ) {
        if sweeps.is_empty() || buffer.is_empty() {
            self.has_data = false;
            return;
        }

        self.site_lat = site_lat;
        self.site_lon = site_lon;
        self.sweep_count = sweeps.len().min(MAX_SWEEPS) as i32;

        let bytes_per_value = word_size as usize;

        // Determine texture dimensions
        let total_values = buffer.len() / bytes_per_value;
        let tex_width = 4096i32;
        let tex_height = ((total_values as i32 + tex_width - 1) / tex_width).max(1);
        self.tex_width = tex_width;

        // Pad buffer to fill full texture
        let padded_size = (tex_width * tex_height) as usize * bytes_per_value;
        let mut padded = buffer.to_vec();
        padded.resize(padded_size, 0);

        // Choose texture format based on word size
        let (internal_fmt, data_type) = if word_size == 1 {
            (glow::R8UI as i32, glow::UNSIGNED_BYTE)
        } else {
            (glow::R16UI as i32, glow::UNSIGNED_SHORT)
        };

        unsafe {
            // Delete old texture
            if let Some(tex) = self.volume_texture.take() {
                gl.delete_texture(tex);
            }

            let tex = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal_fmt,
                tex_width,
                tex_height,
                0,
                glow::RED_INTEGER,
                data_type,
                glow::PixelUnpackData::Slice(Some(&padded)),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            self.volume_texture = Some(tex);
        }

        // Upload sweep metadata uniforms
        unsafe {
            gl.use_program(Some(self.program));

            let n = self.sweep_count as usize;
            let mut elev = [0.0f32; MAX_SWEEPS];
            let mut az_count = [0.0f32; MAX_SWEEPS];
            let mut g_count = [0.0f32; MAX_SWEEPS];
            let mut fg_km = [0.0f32; MAX_SWEEPS];
            let mut gi_km = [0.0f32; MAX_SWEEPS];
            let mut max_r = [0.0f32; MAX_SWEEPS];
            let mut d_off = [0i32; MAX_SWEEPS];
            let mut scale = [1.0f32; MAX_SWEEPS];
            let mut offset = [0.0f32; MAX_SWEEPS];

            for (i, s) in sweeps.iter().take(n).enumerate() {
                elev[i] = s.elevation_deg;
                az_count[i] = s.azimuth_count as f32;
                g_count[i] = s.gate_count as f32;
                fg_km[i] = s.first_gate_km;
                gi_km[i] = s.gate_interval_km;
                max_r[i] = s.max_range_km;
                d_off[i] = s.data_offset as i32;
                scale[i] = s.scale;
                offset[i] = s.offset;
            }

            gl.uniform_1_f32_slice(self.u_elevation.as_ref(), &elev[..n]);
            gl.uniform_1_f32_slice(self.u_azimuth_count.as_ref(), &az_count[..n]);
            gl.uniform_1_f32_slice(self.u_gate_count.as_ref(), &g_count[..n]);
            gl.uniform_1_f32_slice(self.u_first_gate_km.as_ref(), &fg_km[..n]);
            gl.uniform_1_f32_slice(self.u_gate_interval_km.as_ref(), &gi_km[..n]);
            gl.uniform_1_f32_slice(self.u_max_range.as_ref(), &max_r[..n]);
            gl.uniform_1_i32_slice(self.u_data_offset.as_ref(), &d_off[..n]);
            gl.uniform_1_f32_slice(self.u_scale.as_ref(), &scale[..n]);
            gl.uniform_1_f32_slice(self.u_offset.as_ref(), &offset[..n]);

            gl.use_program(None);
        }

        self.has_data = true;

        log::debug!(
            "Volume texture: {}x{} ({} values, {} sweeps, {:.1}KB, {})",
            tex_width,
            tex_height,
            total_values,
            self.sweep_count,
            buffer.len() as f64 / 1024.0,
            if word_size == 1 { "R8UI" } else { "R16UI" },
        );
    }

    /// Returns true if volume data has been uploaded.
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// Paint the volume at half resolution into an FBO, then blit to screen.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        gl: &glow::Context,
        camera: &GlobeCamera,
        lut_texture: glow::Texture,
        processing: &RenderProcessing,
        value_min: f32,
        value_range: f32,
        density_cutoff: f32,
        viewport_width: i32,
        viewport_height: i32,
    ) {
        if !self.has_data || self.volume_texture.is_none() {
            return;
        }

        let fbo_w = (viewport_width as u32 / RESOLUTION_DIVISOR).max(1) as i32;
        let fbo_h = (viewport_height as u32 / RESOLUTION_DIVISOR).max(1) as i32;

        unsafe {
            // Resize FBO if needed
            if fbo_w != self.fbo_width || fbo_h != self.fbo_height {
                self.fbo_width = fbo_w;
                self.fbo_height = fbo_h;
                gl.bind_texture(glow::TEXTURE_2D, Some(self.fbo_color));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    fbo_w,
                    fbo_h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.bind_texture(glow::TEXTURE_2D, None);
                log::debug!(
                    "Volume FBO resized to {}x{} (viewport {}x{})",
                    fbo_w,
                    fbo_h,
                    viewport_width,
                    viewport_height
                );
            }

            // Save GL state that egui has set (scissor, viewport)
            let mut saved_viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut saved_viewport);
            let mut saved_scissor = [0i32; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut saved_scissor);
            let scissor_was_enabled = gl.is_enabled(glow::SCISSOR_TEST);

            // ── Pass 1: Render volume into half-res FBO ──────────────
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.viewport(0, 0, fbo_w, fbo_h);
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::BLEND);
            gl.depth_mask(false);

            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.quad_vao));

            // Bind data textures
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, self.volume_texture);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(lut_texture));

            // Camera uniforms
            let vp = camera.view_projection();
            let inv_vp = vp.inverse();
            gl.uniform_matrix_4_f32_slice(
                self.u_inv_view_projection.as_ref(),
                false,
                &inv_vp.to_cols_array(),
            );

            let cam_pos = camera.camera_world_pos();
            gl.uniform_3_f32(self.u_camera_pos.as_ref(), cam_pos.x, cam_pos.y, cam_pos.z);

            // Site position on unit sphere
            let lat_rad = self.site_lat.to_radians();
            let lon_rad = self.site_lon.to_radians();
            let site_x = (lat_rad.cos() * lon_rad.sin()) as f32;
            let site_y = lat_rad.sin() as f32;
            let site_z = (lat_rad.cos() * lon_rad.cos()) as f32;
            gl.uniform_3_f32(self.u_site_pos.as_ref(), site_x, site_y, site_z);
            gl.uniform_1_f32(self.u_site_lat_rad.as_ref(), lat_rad as f32);
            gl.uniform_1_f32(self.u_site_lon_rad.as_ref(), lon_rad as f32);

            // Volume bounds
            let inner_radius = 1.003f32;
            let max_elev_rad = 20.0f32.to_radians();
            let max_range = 300.0f32;
            let max_height_km = max_range * max_elev_rad.sin() + 10.0;
            let outer_radius = inner_radius + max_height_km / 6371.0;

            gl.uniform_1_f32(self.u_inner_radius.as_ref(), inner_radius);
            gl.uniform_1_f32(self.u_outer_radius.as_ref(), outer_radius);
            gl.uniform_1_f32(self.u_max_range_km.as_ref(), 300.0);

            gl.uniform_1_i32(self.u_tex_width.as_ref(), self.tex_width);
            gl.uniform_1_i32(self.u_sweep_count.as_ref(), self.sweep_count);

            gl.uniform_1_f32(self.u_opacity.as_ref(), processing.opacity);
            gl.uniform_1_f32(self.u_density_cutoff.as_ref(), density_cutoff);
            gl.uniform_1_f32(self.u_value_min.as_ref(), value_min);
            gl.uniform_1_f32(self.u_value_range.as_ref(), value_range);

            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            // ── Pass 2: Blit FBO to screen ───────────────────────────
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            // Restore original viewport and scissor for the blit
            gl.viewport(
                saved_viewport[0],
                saved_viewport[1],
                saved_viewport[2],
                saved_viewport[3],
            );
            if scissor_was_enabled {
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(
                    saved_scissor[0],
                    saved_scissor[1],
                    saved_scissor[2],
                    saved_scissor[3],
                );
            }

            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.disable(glow::DEPTH_TEST);

            gl.use_program(Some(self.blit_program));
            gl.bind_vertex_array(Some(self.quad_vao));

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.fbo_color));

            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            // Cleanup
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
            gl.depth_mask(true);
        }
    }
}

fn cast_f32_to_u8(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}
