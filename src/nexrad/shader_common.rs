pub const FRAGMENT_PREAMBLE: &str = "\
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

pub const SAMPLE_DATA_P: &str = "\
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

pub const IS_VALID: &str = "\
// Raw values 0 (below threshold) and 1 (range folded) are sentinels.
bool is_valid(float v) {
    return v > 1.5;
}
";

pub const FIND_NEAREST_AZ_P: &str = "\
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

pub const FIND_BRACKET_AZ_P: &str = "\
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

pub const RAW_TO_PHYSICAL: &str = "\
    // Convert raw value to physical units
    float physical;
    if (s_scale == 0.0) {
        physical = value;
    } else {
        physical = (value - s_offset) / s_scale;
    }
";

pub const COLOR_LOOKUP: &str = "\
    // Normalize and look up color
    float normalized = clamp((physical - u_value_min) / u_value_range, 0.0, 1.0);
    vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));
";

pub const PREMULTIPLIED_ALPHA_OUTPUT: &str = "\
    // Premultiplied alpha output
    float a = color.a * u_opacity;
    fragColor = vec4(color.rgb * a, a);
";
