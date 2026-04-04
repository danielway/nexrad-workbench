//! Shader source strings and program builder for the GPU radar renderer.
//!
//! Contains all GLSL constants (absorbed from the former `shader_common` module)
//! and the flat-mode fragment shader builder.

use super::RadarGpuRenderer;
use glow::HasContext;
use std::sync::Arc;

// ============================================================================
// Shared GLSL constants (used by both flat and globe renderers)
// ============================================================================

pub(crate) const FRAGMENT_PREAMBLE: &str = "\
uniform float u_gate_count;
uniform float u_azimuth_count;
uniform float u_first_gate_km;
uniform float u_gate_interval_km;
uniform float u_max_range_km;
uniform float u_value_min;
uniform float u_value_range;

uniform sampler2D u_data_tex;
uniform sampler2D u_lut_tex;
uniform sampler2D u_azimuth_tex;

uniform int u_interpolation;
uniform int u_despeckle_enabled;
uniform int u_despeckle_threshold;
uniform float u_opacity;

uniform float u_offset;
uniform float u_scale;

const float PI = 3.14159265359;
";

pub(crate) const SAMPLE_DATA_P: &str = "\
// Sample a data texture at a given (gate_index, azimuth_index).
// Parameterized by sampler and dimensions so the same function works for
// both current and previous sweep textures.
float sample_data_p(sampler2D data_tex, float gate_count, float azimuth_count, float g, float a) {
    if (g < 0.0 || g >= gate_count || a < 0.0 || a >= azimuth_count) {
        return 0.0;
    }
    float gu = (g + 0.5) / gate_count;
    float av = (a + 0.5) / azimuth_count;
    return texture(data_tex, vec2(gu, av)).r;
}
";

pub(crate) const IS_VALID: &str = "\
// Raw values 0 (below threshold) and 1 (range folded) are sentinels.
bool is_valid(float v) {
    return v > 1.5;
}
";

pub(crate) const FIND_NEAREST_AZ_P: &str = "\
// Find the nearest azimuth index for a given angle in degrees.
// Parameterized by azimuth count and sampler.
// Returns -1.0 if no radial is close enough (gap).
float find_nearest_az_p(float azimuth_deg, float az_count, sampler2D az_tex, out float out_az) {
    float az_spacing = 360.0 / az_count;
    float est_idx = azimuth_deg / az_spacing;
    float inv_count = 1.0 / az_count;

    float best_idx = 0.0;
    float best_dist = 360.0;
    float best_az = 0.0;

    for (float offset = -2.0; offset <= 2.0; offset += 1.0) {
        float i = floor(mod(est_idx + offset, az_count));
        float tex_az = texture(az_tex, vec2((i + 0.5) * inv_count, 0.5)).r;
        float d = abs(azimuth_deg - tex_az);
        d = min(d, 360.0 - d);
        if (d < best_dist) {
            best_dist = d;
            best_idx = i;
            best_az = tex_az;
        }
    }

    if (best_dist > az_spacing * 1.5) {
        out_az = 0.0;
        return -1.0;
    }
    out_az = best_az;
    return best_idx;
}
";

pub(crate) const FIND_BRACKET_AZ_P: &str = "\
// Find the two nearest azimuth indices that bracket the given angle.
// Parameterized by azimuth count and sampler.
// Returns false if in a gap region.
bool find_bracket_az_p(float azimuth_deg, float az_count, sampler2D az_tex,
                       out float idx_lo, out float idx_hi, out float frac) {
    float az_spacing = 360.0 / az_count;
    float est_idx = azimuth_deg / az_spacing;
    float inv_count = 1.0 / az_count;

    float cand_idx[5];
    float cand_az[5];
    for (int k = 0; k < 5; k++) {
        float i = floor(mod(est_idx + float(k - 2), az_count));
        cand_idx[k] = i;
        cand_az[k] = texture(az_tex, vec2((i + 0.5) * inv_count, 0.5)).r;
    }

    float lo_idx = -1.0, hi_idx = -1.0;
    float lo_dist = 360.0, hi_dist = 360.0;

    for (int k = 0; k < 5; k++) {
        float az = cand_az[k];
        float diff = azimuth_deg - az;
        diff = mod(diff + 540.0, 360.0) - 180.0;

        if (diff >= 0.0 && diff < lo_dist) {
            lo_dist = diff;
            lo_idx = cand_idx[k];
        }
        if (diff <= 0.0 && (-diff) < hi_dist) {
            hi_dist = -diff;
            hi_idx = cand_idx[k];
        }
    }

    if (lo_idx < 0.0 || hi_idx < 0.0) return false;

    float span = lo_dist + hi_dist;
    if (span > az_spacing * 1.5) return false;

    idx_lo = lo_idx;
    idx_hi = hi_idx;
    frac = (span > 0.001) ? lo_dist / span : 0.0;
    return true;
}
";

pub(crate) const RAW_TO_PHYSICAL: &str = "\
    // Convert raw value to physical units
    float physical;
    if (s_scale == 0.0) {
        physical = value;
    } else {
        physical = (value - s_offset) / s_scale;
    }
";

pub(crate) const COLOR_LOOKUP: &str = "\
    // Normalize and look up color
    float normalized = clamp((physical - u_value_min) / u_value_range, 0.0, 1.0);
    vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));
";

pub(crate) const PREMULTIPLIED_ALPHA_OUTPUT: &str = "\
    // Premultiplied alpha output
    float a = color.a * u_opacity;
    fragColor = vec4(color.rgb * a, a);
";

// ============================================================================
// Vertex shader
// ============================================================================

pub(super) const VERTEX_SHADER: &str = r#"#version 300 es
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

// ============================================================================
// Fragment shader builder
// ============================================================================

pub(super) fn build_flat_fragment_shader() -> String {
    format!(
        r#"#version 300 es
precision highp float;

in vec2 v_screen_pos;
out vec4 fragColor;

uniform vec2 u_radar_center;       // radar center in screen pixels
uniform float u_radar_radius;      // max coverage radius in screen pixels
{FRAGMENT_PREAMBLE}
// Processing uniforms
uniform int u_data_age_indicator; // 0 or 1: desaturate oldest data behind sweep line

// Sweep animation (dual-texture compositing)
uniform sampler2D u_prev_data_tex;    // previous scan gate values (R32F, texture unit 3)
uniform sampler2D u_prev_azimuth_tex; // previous scan azimuths (R32F, texture unit 4)
uniform int u_sweep_enabled;       // 0 or 1
uniform float u_sweep_azimuth;     // current sweep line angle in degrees
uniform float u_sweep_start;       // azimuth where the sweep began collecting
uniform float u_prev_offset;       // previous scan moment offset
uniform float u_prev_scale;        // previous scan moment scale
uniform float u_sweep_chunk_boundary; // extrapolated sweep position, -1 = disabled
// Previous sweep spatial params (may differ from current sweep)
uniform float u_prev_gate_count;
uniform float u_prev_azimuth_count;
uniform float u_prev_first_gate_km;
uniform float u_prev_gate_interval_km;
uniform float u_prev_max_range_km;

{SAMPLE_DATA_P}
{IS_VALID}
{FIND_NEAREST_AZ_P}
{FIND_BRACKET_AZ_P}
void main() {{
    vec2 delta = v_screen_pos - u_radar_center;
    float dist_px = length(delta);
    float dist_km = (dist_px / u_radar_radius) * u_max_range_km;

    float azimuth_rad = atan(delta.x, -delta.y);
    float azimuth_deg = mod(degrees(azimuth_rad) + 360.0, 360.0);

    // Sweep animation: determine whether to sample previous or current texture.
    // Must be computed before range/gate checks because the two textures may
    // have different spatial extents (e.g. 0.5° at 460 km vs 0.9° at 298 km).
    bool use_prev = false;
    if (u_sweep_enabled == 1) {{
        float swept_arc = mod(u_sweep_azimuth - u_sweep_start, 360.0);
        float pixel_from_start = mod(azimuth_deg - u_sweep_start, 360.0);
        use_prev = (pixel_from_start >= swept_arc);
    }}

    // Live desaturation zones (relative to the estimated antenna position):
    //
    //   [fresh data] ← received data edge ← [gap] ← now line ← [fade 90°] ← [no desat]
    //     saturated         fully desat        gradient→0%        previous sweep
    //
    // The gap is where the antenna has swept but S3 uploads haven't arrived.
    // The fade zone is 90° ahead of the now line in rotation direction —
    // the oldest data from the previous rotation, about to be overwritten.
    float desat_factor = 0.0;
    if (u_sweep_enabled == 1 && u_data_age_indicator == 1 && u_sweep_chunk_boundary >= 0.0) {{
        // Distance behind the now line (opposite rotation direction).
        // 0 = at now line, increases going back through received data.
        float behind_now = mod(u_sweep_chunk_boundary - azimuth_deg + 360.0, 360.0);
        // Gap: angular distance from received data edge to now line.
        float data_to_now = mod(u_sweep_chunk_boundary - u_sweep_azimuth + 360.0, 360.0);
        // Distance ahead of the now line (in rotation direction).
        // 0 = at now line, increases going forward into oldest data.
        float ahead_of_now = mod(azimuth_deg - u_sweep_chunk_boundary + 360.0, 360.0);

        if (behind_now < data_to_now && data_to_now < 180.0) {{
            // In the gap between received data edge and now line
            desat_factor = 0.7;
        }} else if (ahead_of_now > 0.0 && ahead_of_now < 90.0) {{
            // 90° ahead of now line: gradient from strong (near now) to none
            desat_factor = (1.0 - ahead_of_now / 90.0) * 0.7;
        }}
    }}
    // Fallback when no estimated antenna position is available
    if (u_sweep_enabled == 1 && u_data_age_indicator == 1 && u_sweep_chunk_boundary < 0.0) {{
        float age = mod(u_sweep_azimuth - azimuth_deg + 360.0, 360.0) / 360.0;
        desat_factor = clamp((age - 0.75) / 0.25, 0.0, 1.0) * 0.9;
    }}

    // --- Unified sweep pipeline: select spatial params and samplers based on use_prev ---
    float s_gate_count    = use_prev ? u_prev_gate_count    : u_gate_count;
    float s_azimuth_count = use_prev ? u_prev_azimuth_count : u_azimuth_count;
    float s_first_gate_km = use_prev ? u_prev_first_gate_km : u_first_gate_km;
    float s_gate_interval = use_prev ? u_prev_gate_interval_km : u_gate_interval_km;
    float s_max_range_km  = use_prev ? u_prev_max_range_km  : u_max_range_km;
    float s_offset        = use_prev ? u_prev_offset        : u_offset;
    float s_scale         = use_prev ? u_prev_scale         : u_scale;

    // Range check
    if (dist_km < s_first_gate_km || dist_km >= s_max_range_km) {{
        fragColor = vec4(0.0);
        return;
    }}

    float gate_idx = (dist_km - s_first_gate_km) / s_gate_interval;
    if (gate_idx < 0.0 || gate_idx >= s_gate_count) {{
        fragColor = vec4(0.0);
        return;
    }}

    // Select the active samplers for the chosen sweep (current or previous).
    // GLSL ES 3.00 requires sampler access in uniform control flow, but
    // use_prev is uniform across the draw call for a given pixel's branch —
    // we use explicit if/else to satisfy the compiler.
    float value;

    if (u_interpolation == 1) {{
        // Bilinear interpolation
        float az_lo, az_hi, az_frac;
        bool bracket_ok;
        if (use_prev) {{
            bracket_ok = find_bracket_az_p(azimuth_deg, s_azimuth_count, u_prev_azimuth_tex, az_lo, az_hi, az_frac);
        }} else {{
            bracket_ok = find_bracket_az_p(azimuth_deg, s_azimuth_count, u_azimuth_tex, az_lo, az_hi, az_frac);
        }}
        if (!bracket_ok) {{
            fragColor = vec4(0.0);
            return;
        }}

        float g_lo = floor(gate_idx);
        float g_hi = min(g_lo + 1.0, s_gate_count - 1.0);
        float g_frac = gate_idx - g_lo;

        float v00, v10, v01, v11;
        if (use_prev) {{
            v00 = sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, g_lo, az_lo);
            v10 = sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, g_hi, az_lo);
            v01 = sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, g_lo, az_hi);
            v11 = sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, g_hi, az_hi);
        }} else {{
            v00 = sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, g_lo, az_lo);
            v10 = sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, g_hi, az_lo);
            v01 = sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, g_lo, az_hi);
            v11 = sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, g_hi, az_hi);
        }}

        // Weighted average skipping sentinel values
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
            fragColor = vec4(0.0);
            return;
        }}
        value = sum / wsum;
    }} else {{
        // Nearest neighbor
        float dummy_az;
        float best_idx;
        if (use_prev) {{
            best_idx = find_nearest_az_p(azimuth_deg, s_azimuth_count, u_prev_azimuth_tex, dummy_az);
        }} else {{
            best_idx = find_nearest_az_p(azimuth_deg, s_azimuth_count, u_azimuth_tex, dummy_az);
        }}
        if (best_idx < 0.0) {{
            fragColor = vec4(0.0);
            return;
        }}
        if (use_prev) {{
            value = sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, floor(gate_idx), best_idx);
        }} else {{
            value = sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, floor(gate_idx), best_idx);
        }}
    }}

    if (!is_valid(value)) {{
        fragColor = vec4(0.0);
        return;
    }}

    // Despeckle filter
    if (u_despeckle_enabled == 1) {{
        float dummy_az2;
        float center_az;
        if (use_prev) {{
            center_az = find_nearest_az_p(azimuth_deg, s_azimuth_count, u_prev_azimuth_tex, dummy_az2);
        }} else {{
            center_az = find_nearest_az_p(azimuth_deg, s_azimuth_count, u_azimuth_tex, dummy_az2);
        }}
        float center_g = floor(gate_idx);
        if (center_az >= 0.0) {{
            int valid_count = 0;
            for (int dg = -1; dg <= 1; dg++) {{
                for (int da = -1; da <= 1; da++) {{
                    if (dg == 0 && da == 0) continue;
                    float ng = center_g + float(dg);
                    float na = mod(center_az + float(da), s_azimuth_count);
                    if (use_prev) {{
                        if (is_valid(sample_data_p(u_prev_data_tex, s_gate_count, s_azimuth_count, ng, na))) valid_count++;
                    }} else {{
                        if (is_valid(sample_data_p(u_data_tex, s_gate_count, s_azimuth_count, ng, na))) valid_count++;
                    }}
                }}
            }}
            if (valid_count < u_despeckle_threshold) {{
                fragColor = vec4(0.0);
                return;
            }}
        }}
    }}

{RAW_TO_PHYSICAL}
{COLOR_LOOKUP}
    // Apply desaturation
    if (desat_factor > 0.0) {{
        float lum = dot(color.rgb, vec3(0.299, 0.587, 0.114));
        color.rgb = mix(color.rgb, vec3(lum), desat_factor);
    }}

    // Apply opacity and output premultiplied alpha (egui requirement)
    float a = color.a * u_opacity;
    fragColor = vec4(color.rgb * a, a);
}}
"#
    )
}

impl RadarGpuRenderer {
    /// Compile shaders and link the GL program. Returns the program handle.
    ///
    /// Called once from `RadarGpuRenderer::new()`.
    pub(super) fn build_program(gl: &Arc<glow::Context>) -> Result<glow::Program, String> {
        unsafe {
            let program = gl
                .create_program()
                .map_err(|e| format!("Cannot create program: {}", e))?;

            let vert = gl
                .create_shader(glow::VERTEX_SHADER)
                .map_err(|e| format!("Cannot create vertex shader: {}", e))?;
            gl.shader_source(vert, VERTEX_SHADER);
            gl.compile_shader(vert);
            if !gl.get_shader_compile_status(vert) {
                let info = gl.get_shader_info_log(vert);
                return Err(format!("Vertex shader compile error: {}", info));
            }

            let frag = gl
                .create_shader(glow::FRAGMENT_SHADER)
                .map_err(|e| format!("Cannot create fragment shader: {}", e))?;
            let frag_src = build_flat_fragment_shader();
            gl.shader_source(frag, &frag_src);
            gl.compile_shader(frag);
            if !gl.get_shader_compile_status(frag) {
                let info = gl.get_shader_info_log(frag);
                return Err(format!("Fragment shader compile error: {}", info));
            }

            gl.attach_shader(program, vert);
            gl.attach_shader(program, frag);
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let info = gl.get_program_info_log(program);
                return Err(format!("Shader program link error: {}", info));
            }
            gl.detach_shader(program, vert);
            gl.detach_shader(program, frag);
            gl.delete_shader(vert);
            gl.delete_shader(frag);

            Ok(program)
        }
    }
}
