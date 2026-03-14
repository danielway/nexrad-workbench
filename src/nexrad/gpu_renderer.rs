//! GPU-based radar renderer using WebGL2 shaders via glow.
//!
//! Renders polar radar data (azimuths x gates) directly on the GPU using a fragment
//! shader that performs polar-to-Cartesian conversion and color lookup from a LUT texture.

use crate::state::RenderProcessing;
use glow::HasContext;
use nexrad_render::{Color as NrColor, ColorScale, ColorStop, ContinuousColorScale, Product};
use std::sync::Arc;

// Default value ranges per product (used for color LUT normalization).
pub fn product_value_range(product: Product) -> (f32, f32) {
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

// --- OKLab color space helpers for perceptually uniform interpolation ---

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert sRGB (0-1) to OKLab (L, a, b).
#[allow(clippy::excessive_precision)]
fn srgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let lr = srgb_to_linear(r);
    let lg = srgb_to_linear(g);
    let lb = srgb_to_linear(b);

    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )
}

/// Convert OKLab (L, a, b) to sRGB (0-1), clamped.
#[allow(clippy::excessive_precision)]
fn oklab_to_srgb(ol: f32, oa: f32, ob: f32) -> (f32, f32, f32) {
    let l_ = ol + 0.3963377774 * oa + 0.2158037573 * ob;
    let m_ = ol - 0.1055613458 * oa - 0.0638541728 * ob;
    let s_ = ol - 0.0894841775 * oa - 1.2914855480 * ob;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;

    (
        linear_to_srgb(r.clamp(0.0, 1.0)),
        linear_to_srgb(g.clamp(0.0, 1.0)),
        linear_to_srgb(b.clamp(0.0, 1.0)),
    )
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Build a 1024-entry RGBA LUT for reflectivity using OKLab interpolation,
/// monotonically increasing luminance, and a low-end alpha ramp.
pub fn build_reflectivity_lut(min_val: f32, max_val: f32) -> Vec<u8> {
    // Anchor colors: (dBZ, r, g, b, a) in sRGB 0-1.
    // Designed for black background with increasing luminance.
    let anchors: &[(f32, f32, f32, f32, f32)] = &[
        (0.0, 0.00, 0.00, 0.00, 0.00),  // black
        (5.0, 0.10, 0.10, 0.14, 0.15),  // near-black, faint
        (10.0, 0.20, 0.22, 0.32, 0.40), // dim blue-grey
        (15.0, 0.35, 0.40, 0.58, 0.75), // slate blue
        (20.0, 0.15, 0.72, 0.15, 0.90), // SHARP bright green
        (28.0, 0.05, 0.35, 0.08, 1.00), // dark green
        (32.0, 0.90, 0.88, 0.10, 1.00), // SHARP bright yellow
        (37.0, 0.68, 0.64, 0.10, 1.00), // duller yellow
        (40.0, 0.92, 0.58, 0.08, 1.00), // SHARP bright orange
        (45.0, 0.70, 0.45, 0.06, 1.00), // warm dark yellow-orange
        (50.0, 0.85, 0.12, 0.10, 1.00), // SHARP bright red
        (55.0, 0.52, 0.10, 0.08, 1.00), // dark red
        (60.0, 0.92, 0.68, 0.72, 1.00), // SHARP blush
        (65.0, 0.95, 0.25, 0.55, 1.00), // hot pink
        (70.0, 0.68, 0.20, 0.85, 1.00), // SHARP bright purple
        (75.0, 0.28, 0.08, 0.40, 1.00), // dark purple
        (80.0, 0.20, 0.82, 0.85, 1.00), // SHARP bright cyan
        (90.0, 0.08, 0.30, 0.34, 1.00), // dark cyan
    ];

    // Pre-convert anchors to OKLab
    let oklab_anchors: Vec<(f32, f32, f32, f32, f32)> = anchors
        .iter()
        .map(|&(dbz, r, g, b, a)| {
            let (ol, oa, ob) = srgb_to_oklab(r, g, b);
            (dbz, ol, oa, ob, a)
        })
        .collect();

    let lut_size = 1024usize;
    let mut lut_data = Vec::with_capacity(lut_size * 4);
    let range = max_val - min_val;

    for i in 0..lut_size {
        let t = i as f32 / (lut_size - 1) as f32;
        // Optional perceptual shaping: slightly expand midrange
        let t_shaped = t.powf(0.9);
        let dbz = min_val + t_shaped * range;

        // Find bracketing anchors
        let mut seg = 0usize;
        #[allow(clippy::needless_range_loop)]
        for j in 1..oklab_anchors.len() {
            if oklab_anchors[j].0 >= dbz {
                seg = j - 1;
                break;
            }
            seg = j - 1;
        }
        let lo = &oklab_anchors[seg];
        let hi = if seg + 1 < oklab_anchors.len() {
            &oklab_anchors[seg + 1]
        } else {
            lo
        };

        let frac = if (hi.0 - lo.0).abs() > 0.001 {
            ((dbz - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // Interpolate in OKLab space
        let ol = lo.1 + frac * (hi.1 - lo.1);
        let oa = lo.2 + frac * (hi.2 - lo.2);
        let ob = lo.3 + frac * (hi.3 - lo.3);
        let alpha_anchor = lo.4 + frac * (hi.4 - lo.4);

        let (r, g, b) = oklab_to_srgb(ol, oa, ob);

        // Low-end alpha ramp: smoothstep from 3 to 12 dBZ
        let alpha_ramp = smoothstep(3.0, 12.0, dbz);
        let alpha = (alpha_anchor * alpha_ramp).clamp(0.0, 1.0);

        lut_data.push((r * 255.0).round() as u8);
        lut_data.push((g * 255.0).round() as u8);
        lut_data.push((b * 255.0).round() as u8);
        lut_data.push((alpha * 255.0).round() as u8);
    }

    lut_data
}

/// Build a continuous (linearly-interpolated) color scale for the given product.
/// Uses the same color stops as the discrete NWS scales from the nexrad_render crate,
/// but interpolates smoothly between them for a commercial-quality appearance.
pub fn continuous_color_scale(product: Product) -> ColorScale {
    let stops = match product {
        Product::Reflectivity => vec![
            // Unused for LUT generation (build_reflectivity_lut is used instead),
            // but kept as fallback for color_scale.color() calls.
            ColorStop::new(0.0, NrColor::rgba(0.0000, 0.0000, 0.0000, 0.0)),
            ColorStop::new(5.0, NrColor::rgba(0.04, 0.14, 0.18, 0.15)),
            ColorStop::new(15.0, NrColor::rgba(0.08, 0.35, 0.30, 0.70)),
            ColorStop::new(25.0, NrColor::rgba(0.22, 0.56, 0.16, 0.95)),
            ColorStop::new(35.0, NrColor::rgb(0.62, 0.64, 0.10)),
            ColorStop::new(45.0, NrColor::rgb(0.82, 0.48, 0.10)),
            ColorStop::new(55.0, NrColor::rgb(0.80, 0.14, 0.16)),
            ColorStop::new(65.0, NrColor::rgb(0.70, 0.22, 0.56)),
            ColorStop::new(75.0, NrColor::rgb(0.88, 0.65, 0.80)),
        ],
        Product::Velocity => vec![
            ColorStop::new(-64.0, NrColor::rgb(0.00, 0.15, 0.00)), // very dark green
            ColorStop::new(-50.0, NrColor::rgb(0.00, 0.38, 0.00)), // dark green
            ColorStop::new(-36.0, NrColor::rgb(0.00, 0.65, 0.00)), // medium green
            ColorStop::new(-26.0, NrColor::rgb(0.00, 0.90, 0.00)), // bright green
            ColorStop::new(-16.0, NrColor::rgb(0.45, 0.95, 0.35)), // light green
            ColorStop::new(-5.0, NrColor::rgb(0.68, 0.78, 0.68)),  // gray-green
            ColorStop::new(0.0, NrColor::rgb(0.60, 0.60, 0.60)),   // neutral gray
            ColorStop::new(5.0, NrColor::rgb(0.78, 0.68, 0.68)),   // gray-red
            ColorStop::new(16.0, NrColor::rgb(0.95, 0.40, 0.35)),  // light red
            ColorStop::new(26.0, NrColor::rgb(0.90, 0.00, 0.00)),  // bright red
            ColorStop::new(36.0, NrColor::rgb(0.65, 0.00, 0.00)),  // medium red
            ColorStop::new(50.0, NrColor::rgb(0.40, 0.00, 0.00)),  // dark red
            ColorStop::new(64.0, NrColor::rgb(0.18, 0.00, 0.00)),  // very dark red
        ],
        Product::SpectrumWidth => vec![
            ColorStop::new(0.0, NrColor::rgb(0.5020, 0.5020, 0.5020)),
            ColorStop::new(4.0, NrColor::rgb(0.0000, 0.0000, 0.8039)),
            ColorStop::new(8.0, NrColor::rgb(0.0000, 0.8039, 0.8039)),
            ColorStop::new(12.0, NrColor::rgb(0.0000, 0.8039, 0.0000)),
            ColorStop::new(16.0, NrColor::rgb(0.9333, 0.9333, 0.0000)),
            ColorStop::new(20.0, NrColor::rgb(1.0000, 0.6471, 0.0000)),
            ColorStop::new(25.0, NrColor::rgb(1.0000, 0.0000, 0.0000)),
        ],
        Product::DifferentialReflectivity => vec![
            ColorStop::new(-2.0, NrColor::rgb(0.5020, 0.0000, 0.5020)),
            ColorStop::new(-1.0, NrColor::rgb(0.0000, 0.0000, 0.8039)),
            ColorStop::new(0.0, NrColor::rgb(0.6627, 0.6627, 0.6627)),
            ColorStop::new(0.5, NrColor::rgb(0.5647, 0.9333, 0.5647)),
            ColorStop::new(1.5, NrColor::rgb(0.9333, 0.9333, 0.0000)),
            ColorStop::new(2.5, NrColor::rgb(1.0000, 0.6471, 0.0000)),
            ColorStop::new(4.0, NrColor::rgb(1.0000, 0.0000, 0.0000)),
        ],
        Product::CorrelationCoefficient => vec![
            ColorStop::new(0.0, NrColor::rgb(0.0000, 0.0000, 0.0000)),
            ColorStop::new(0.2, NrColor::rgb(0.3922, 0.0000, 0.5882)),
            ColorStop::new(0.5, NrColor::rgb(0.0000, 0.0000, 0.8039)),
            ColorStop::new(0.7, NrColor::rgb(0.0000, 0.5451, 0.5451)),
            ColorStop::new(0.85, NrColor::rgb(0.0000, 0.8039, 0.4000)),
            ColorStop::new(0.92, NrColor::rgb(0.0000, 0.8039, 0.0000)),
            ColorStop::new(0.96, NrColor::rgb(0.9333, 0.9333, 0.0000)),
            ColorStop::new(0.98, NrColor::rgb(0.9020, 0.9020, 0.9020)),
        ],
        Product::DifferentialPhase => vec![
            ColorStop::new(0.0, NrColor::rgb(0.5020, 0.0000, 0.5020)),
            ColorStop::new(45.0, NrColor::rgb(0.0000, 0.0000, 0.8039)),
            ColorStop::new(90.0, NrColor::rgb(0.0000, 0.8039, 0.8039)),
            ColorStop::new(135.0, NrColor::rgb(0.0000, 0.8039, 0.0000)),
            ColorStop::new(180.0, NrColor::rgb(0.9333, 0.9333, 0.0000)),
            ColorStop::new(225.0, NrColor::rgb(1.0000, 0.6471, 0.0000)),
            ColorStop::new(270.0, NrColor::rgb(1.0000, 0.0000, 0.0000)),
            ColorStop::new(315.0, NrColor::rgb(1.0000, 0.0000, 1.0000)),
        ],
        Product::ClutterFilterPower => vec![
            ColorStop::new(-20.0, NrColor::rgb(0.0000, 0.0000, 0.5451)),
            ColorStop::new(-10.0, NrColor::rgb(0.0000, 0.0000, 0.8039)),
            ColorStop::new(-5.0, NrColor::rgb(0.6784, 0.8471, 0.9020)),
            ColorStop::new(-1.0, NrColor::rgb(0.6627, 0.6627, 0.6627)),
            ColorStop::new(1.0, NrColor::rgb(0.6627, 0.6627, 0.6627)),
            ColorStop::new(5.0, NrColor::rgb(1.0000, 0.7529, 0.7961)),
            ColorStop::new(10.0, NrColor::rgb(1.0000, 0.4118, 0.4118)),
            ColorStop::new(20.0, NrColor::rgb(0.8039, 0.0000, 0.0000)),
        ],
    };
    ColorScale::Continuous(ContinuousColorScale::new(stops))
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

// Processing uniforms
uniform int u_interpolation;       // 0=nearest, 1=bilinear
uniform int u_smoothing_enabled;   // 0 or 1
uniform float u_smoothing_radius;  // kernel radius in samples
uniform int u_despeckle_enabled;   // 0 or 1
uniform int u_despeckle_threshold; // min valid neighbors to keep
uniform float u_opacity;           // global alpha multiplier
uniform int u_edge_softening;     // 0 or 1: smooth alpha falloff at echo boundaries

// Raw-to-physical conversion: physical = (raw - u_offset) / u_scale
uniform float u_offset;            // moment offset
uniform float u_scale;             // moment scale (0.0 = raw values are physical)

// Sweep animation (dual-texture compositing)
uniform sampler2D u_prev_data_tex; // previous scan gate values (R32F, texture unit 3)
uniform int u_sweep_enabled;       // 0 or 1
uniform float u_sweep_azimuth;     // current sweep line angle in degrees
uniform float u_sweep_start;       // azimuth where the sweep began collecting
uniform float u_prev_offset;       // previous scan moment offset
uniform float u_prev_scale;        // previous scan moment scale
// Previous sweep spatial params (may differ from current sweep)
uniform float u_prev_gate_count;
uniform float u_prev_azimuth_count;
uniform float u_prev_first_gate_km;
uniform float u_prev_gate_interval_km;
uniform float u_prev_max_range_km;

const float PI = 3.14159265359;

// Sample the raw data texture at a given (gate_index, azimuth_index).
// Returns 0.0 for out-of-range (sentinel: below threshold).
float sample_data(float g, float a) {
    if (g < 0.0 || g >= u_gate_count || a < 0.0 || a >= u_azimuth_count) {
        return 0.0;
    }
    float gu = (g + 0.5) / u_gate_count;
    float av = (a + 0.5) / u_azimuth_count;
    return texture(u_data_tex, vec2(gu, av)).r;
}

// Sample the previous scan's data texture at a given (gate_index, azimuth_index).
// Uses prev-texture dimensions for correct UV mapping when sweeps differ.
float sample_prev_data(float g, float a) {
    if (g < 0.0 || g >= u_prev_gate_count || a < 0.0 || a >= u_prev_azimuth_count) {
        return 0.0;
    }
    float gu = (g + 0.5) / u_prev_gate_count;
    float av = (a + 0.5) / u_prev_azimuth_count;
    return texture(u_prev_data_tex, vec2(gu, av)).r;
}

// Raw values 0 (below threshold) and 1 (range folded) are sentinels.
bool is_valid(float v) {
    return v > 1.5;
}

// Find the nearest azimuth index for a given angle in degrees.
// Returns -1.0 if no radial is close enough (gap).
// Also writes the azimuth angle of the found radial to out_az.
float find_nearest_az(float azimuth_deg, out float out_az) {
    float az_spacing = 360.0 / u_azimuth_count;
    float est_idx = azimuth_deg / az_spacing;
    float inv_count = 1.0 / u_azimuth_count;

    float best_idx = 0.0;
    float best_dist = 360.0;
    float best_az = 0.0;

    for (float offset = -2.0; offset <= 2.0; offset += 1.0) {
        float i = floor(mod(est_idx + offset, u_azimuth_count));
        float tex_az = texture(u_azimuth_tex, vec2((i + 0.5) * inv_count, 0.5)).r;
        float d = abs(azimuth_deg - tex_az);
        d = min(d, 360.0 - d);
        if (d < best_dist) {
            best_dist = d;
            best_idx = i;
            best_az = tex_az;
        }
    }

    // Gap detection
    if (best_dist > az_spacing * 1.5) {
        out_az = 0.0;
        return -1.0;
    }
    out_az = best_az;
    return best_idx;
}

// Find the two nearest azimuth indices that bracket the given angle.
// Returns false if in a gap region.
bool find_bracket_az(float azimuth_deg, out float idx_lo, out float idx_hi, out float frac) {
    float az_spacing = 360.0 / u_azimuth_count;
    float est_idx = azimuth_deg / az_spacing;
    float inv_count = 1.0 / u_azimuth_count;

    // Collect candidates (up to 5)
    float cand_idx[5];
    float cand_az[5];
    for (int k = 0; k < 5; k++) {
        float i = floor(mod(est_idx + float(k - 2), u_azimuth_count));
        cand_idx[k] = i;
        cand_az[k] = texture(u_azimuth_tex, vec2((i + 0.5) * inv_count, 0.5)).r;
    }

    // Find the closest radial on each side (or equal)
    float lo_idx = -1.0, hi_idx = -1.0;
    float lo_az = -999.0, hi_az = 999.0;
    float lo_dist = 360.0, hi_dist = 360.0;

    for (int k = 0; k < 5; k++) {
        float az = cand_az[k];
        // Signed angular difference (target - candidate), wrapped to -180..180
        float diff = azimuth_deg - az;
        diff = mod(diff + 540.0, 360.0) - 180.0;

        if (diff >= 0.0 && diff < lo_dist) {
            lo_dist = diff;
            lo_idx = cand_idx[k];
            lo_az = az;
        }
        if (diff <= 0.0 && (-diff) < hi_dist) {
            hi_dist = -diff;
            hi_idx = cand_idx[k];
            hi_az = az;
        }
    }

    if (lo_idx < 0.0 || hi_idx < 0.0) return false;

    // Gap check
    float span = lo_dist + hi_dist;
    if (span > az_spacing * 1.5) return false;

    idx_lo = lo_idx;
    idx_hi = hi_idx;
    frac = (span > 0.001) ? lo_dist / span : 0.0;
    return true;
}

void main() {
    vec2 delta = v_screen_pos - u_radar_center;
    float dist_px = length(delta);
    float dist_km = (dist_px / u_radar_radius) * u_max_range_km;

    float azimuth_rad = atan(delta.x, -delta.y);
    float azimuth_deg = mod(degrees(azimuth_rad) + 360.0, 360.0);

    // Sweep animation: determine whether to sample previous or current texture.
    // Must be computed before range/gate checks because the two textures may
    // have different spatial extents (e.g. 0.5° at 460 km vs 0.9° at 298 km).
    bool use_prev = false;
    if (u_sweep_enabled == 1) {
        float swept_arc = mod(u_sweep_azimuth - u_sweep_start, 360.0);
        float pixel_from_start = mod(azimuth_deg - u_sweep_start, 360.0);
        use_prev = (pixel_from_start >= swept_arc);
    }

    // --- Previous sweep: self-contained branch with its own spatial params ---
    // Uses nearest-neighbor with angle-based azimuth index (no LUT needed for
    // the background layer). This correctly handles sweeps with different gate
    // counts, range extents, and azimuth counts.
    if (use_prev) {
        if (dist_km < u_prev_first_gate_km || dist_km >= u_prev_max_range_km) {
            fragColor = vec4(0.0);
            return;
        }
        float prev_gate_idx = (dist_km - u_prev_first_gate_km) / u_prev_gate_interval_km;
        if (prev_gate_idx < 0.0 || prev_gate_idx >= u_prev_gate_count) {
            fragColor = vec4(0.0);
            return;
        }
        // Compute azimuth index from angle (evenly-spaced assumption — accurate for NEXRAD)
        float prev_az_idx = floor(mod(azimuth_deg * u_prev_azimuth_count / 360.0, u_prev_azimuth_count));
        float value = sample_prev_data(floor(prev_gate_idx), prev_az_idx);

        if (!is_valid(value)) {
            fragColor = vec4(0.0);
            return;
        }

        float physical;
        if (u_prev_scale == 0.0) {
            physical = value;
        } else {
            physical = (value - u_prev_offset) / u_prev_scale;
        }

        float normalized = clamp((physical - u_value_min) / u_value_range, 0.0, 1.0);
        vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));
        float a = color.a * u_opacity;
        fragColor = vec4(color.rgb * a, a);
        return;
    }

    // --- Current sweep: full pipeline with bilinear, despeckle, smoothing ---
    if (dist_km < u_first_gate_km || dist_km >= u_max_range_km) {
        fragColor = vec4(0.0);
        return;
    }

    float gate_idx = (dist_km - u_first_gate_km) / u_gate_interval_km;

    if (gate_idx < 0.0 || gate_idx >= u_gate_count) {
        fragColor = vec4(0.0);
        return;
    }

    float value;
    float edge_alpha = 1.0;

    if (u_interpolation == 1) {
        // ---- Bilinear interpolation ----
        float az_lo, az_hi, az_frac;
        if (!find_bracket_az(azimuth_deg, az_lo, az_hi, az_frac)) {
            fragColor = vec4(0.0);
            return;
        }

        float g_lo = floor(gate_idx);
        float g_hi = min(g_lo + 1.0, u_gate_count - 1.0);
        float g_frac = gate_idx - g_lo;

        float v00 = sample_data(g_lo, az_lo);
        float v10 = sample_data(g_hi, az_lo);
        float v01 = sample_data(g_lo, az_hi);
        float v11 = sample_data(g_hi, az_hi);

        // Weighted average skipping SENTINEL values
        float sum = 0.0;
        float wsum = 0.0;
        float w00 = (1.0 - g_frac) * (1.0 - az_frac);
        float w10 = g_frac * (1.0 - az_frac);
        float w01 = (1.0 - g_frac) * az_frac;
        float w11 = g_frac * az_frac;

        if (is_valid(v00)) { sum += v00 * w00; wsum += w00; }
        if (is_valid(v10)) { sum += v10 * w10; wsum += w10; }
        if (is_valid(v01)) { sum += v01 * w01; wsum += w01; }
        if (is_valid(v11)) { sum += v11 * w11; wsum += w11; }

        if (wsum < 0.001) {
            fragColor = vec4(0.0);
            return;
        }
        value = sum / wsum;

        // Edge softening: fade alpha at echo boundaries where some neighbors are invalid
        if (u_edge_softening == 1) {
            edge_alpha = clamp(wsum * 1.5, 0.0, 1.0);
        }
    } else {
        // ---- Nearest neighbor (original) ----
        float dummy_az;
        float best_idx = find_nearest_az(azimuth_deg, dummy_az);
        if (best_idx < 0.0) {
            fragColor = vec4(0.0);
            return;
        }
        value = sample_data(floor(gate_idx), best_idx);
    }

    if (!is_valid(value)) {
        fragColor = vec4(0.0);
        return;
    }

    // ---- Despeckle filter ----
    if (u_despeckle_enabled == 1) {
        float dummy_az2;
        float center_az = find_nearest_az(azimuth_deg, dummy_az2);
        float center_g = floor(gate_idx);
        if (center_az >= 0.0) {
            int valid_count = 0;
            for (int dg = -1; dg <= 1; dg++) {
                for (int da = -1; da <= 1; da++) {
                    if (dg == 0 && da == 0) continue;
                    float ng = center_g + float(dg);
                    float na = mod(center_az + float(da), u_azimuth_count);
                    if (is_valid(sample_data(ng, na))) {
                        valid_count++;
                    }
                }
            }
            if (valid_count < u_despeckle_threshold) {
                fragColor = vec4(0.0);
                return;
            }
        }
    }

    // ---- Gaussian smoothing ----
    if (u_smoothing_enabled == 1) {
        float dummy_az3;
        float center_az = find_nearest_az(azimuth_deg, dummy_az3);
        float center_g = floor(gate_idx);
        if (center_az >= 0.0) {
            float sigma = u_smoothing_radius * 0.5;
            float sigma2 = 2.0 * sigma * sigma;
            int r = int(ceil(u_smoothing_radius));
            float wsum = 0.0;
            float vsum = 0.0;
            for (int dg = -r; dg <= r; dg++) {
                for (int da = -r; da <= r; da++) {
                    float ng = center_g + float(dg);
                    float na = mod(center_az + float(da), u_azimuth_count);
                    float sv = sample_data(ng, na);
                    if (is_valid(sv)) {
                        // Range-aware: scale azimuthal distance by normalized range
                        // so smoothing is spatially uniform (azimuths are wider at far range)
                        float range_norm = max(center_g / u_gate_count, 0.1);
                        float d2 = float(dg * dg) + float(da * da) * (range_norm * range_norm);
                        float w = exp(-d2 / sigma2);
                        vsum += sv * w;
                        wsum += w;
                    }
                }
            }
            if (wsum > 0.001) {
                value = vsum / wsum;
            }
        }
    }

    // Convert raw value to physical units
    float physical;
    if (u_scale == 0.0) {
        physical = value;           // raw values are already physical
    } else {
        physical = (value - u_offset) / u_scale;
    }

    // Normalize and look up color
    float normalized = clamp((physical - u_value_min) / u_value_range, 0.0, 1.0);
    vec4 color = texture(u_lut_tex, vec2(normalized, 0.5));

    // Apply opacity, edge softening, and output premultiplied alpha (egui requirement)
    float a = color.a * u_opacity * edge_alpha;
    fragColor = vec4(color.rgb * a, a);
}
"#;

/// GPU-based radar renderer using WebGL2 shaders.
pub struct RadarGpuRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    #[allow(dead_code)] // Retained to prevent GPU resource deallocation
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

    // Processing uniform locations
    u_interpolation: glow::UniformLocation,
    u_smoothing_enabled: glow::UniformLocation,
    u_smoothing_radius: glow::UniformLocation,
    u_despeckle_enabled: glow::UniformLocation,
    u_despeckle_threshold: glow::UniformLocation,
    u_opacity: glow::UniformLocation,
    u_edge_softening: glow::UniformLocation,

    // Raw-to-physical conversion uniform locations
    u_offset: glow::UniformLocation,
    u_scale: glow::UniformLocation,

    // Sweep animation uniform locations
    u_sweep_enabled: glow::UniformLocation,
    u_sweep_azimuth: glow::UniformLocation,
    u_sweep_start: glow::UniformLocation,
    u_prev_offset: glow::UniformLocation,
    u_prev_scale: glow::UniformLocation,
    u_prev_gate_count: glow::UniformLocation,
    u_prev_azimuth_count: glow::UniformLocation,
    u_prev_first_gate_km: glow::UniformLocation,
    u_prev_gate_interval_km: glow::UniformLocation,
    u_prev_max_range_km: glow::UniformLocation,

    // Previous scan texture (for sweep animation)
    prev_data_texture: glow::Texture,
    prev_data_offset: f32,
    prev_data_scale: f32,
    prev_azimuth_count: u32,
    prev_gate_count: u32,
    prev_first_gate_km: f64,
    prev_gate_interval_km: f64,
    prev_max_range_km: f64,
    /// Identity of the sweep currently in `prev_data_texture` (scan_key|elev_num).
    prev_sweep_id: Option<String>,

    // Data metadata
    azimuth_count: u32,
    gate_count: u32,
    first_gate_km: f64,
    gate_interval_km: f64,
    max_range_km: f64,
    value_min: f32,
    value_range: f32,
    has_data: bool,

    // Raw-to-physical conversion params (for CPU-side inspector/storm detection)
    data_offset: f32,
    data_scale: f32,

    /// Identity of the sweep currently in `data_texture` (scan_key|elev_num).
    current_sweep_id: Option<String>,

    // CPU-side copies for inspector value lookup
    cpu_azimuths: Vec<f32>,
    cpu_gate_values: Vec<f32>,
    /// Per-radial collection timestamps in Unix seconds (parallel to cpu_azimuths).
    cpu_radial_times: Vec<f64>,
}

impl RadarGpuRenderer {
    /// Create a new GPU renderer, compiling shaders and allocating GL resources.
    ///
    /// Returns `Err` if shader compilation, program linking, or GL resource
    /// allocation fails, allowing the caller to fall back gracefully.
    pub fn new(gl: &Arc<glow::Context>) -> Result<Self, String> {
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
            gl.shader_source(frag, FRAGMENT_SHADER);
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

            // Fullscreen quad (two triangles)
            let vertices: [f32; 12] = [
                -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0,
            ];

            let vbo = gl
                .create_buffer()
                .map_err(|e| format!("Cannot create VBO: {}", e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast_slice(&vertices),
                glow::STATIC_DRAW,
            );

            let vao = gl
                .create_vertex_array()
                .map_err(|e| format!("Cannot create VAO: {}", e))?;
            gl.bind_vertex_array(Some(vao));
            let a_position = gl
                .get_attrib_location(program, "a_position")
                .ok_or("Missing a_position")?;
            gl.enable_vertex_attrib_array(a_position);
            gl.vertex_attrib_pointer_f32(a_position, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);

            // Create placeholder textures (1x1)
            let data_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let azimuth_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let lut_texture = create_rgba8_texture(gl, 1, 1, &[0, 0, 0, 0]);

            // Helper to look up a required uniform location
            let uniform = |name: &str| -> Result<glow::UniformLocation, String> {
                gl.get_uniform_location(program, name)
                    .ok_or_else(|| format!("Missing uniform: {}", name))
            };

            // Bind texture units to sampler uniforms
            gl.use_program(Some(program));

            let u_data_tex = uniform("u_data_tex")?;
            gl.uniform_1_i32(Some(&u_data_tex), 0);
            let u_lut_tex = uniform("u_lut_tex")?;
            gl.uniform_1_i32(Some(&u_lut_tex), 1);
            let u_azimuth_tex = uniform("u_azimuth_tex")?;
            gl.uniform_1_i32(Some(&u_azimuth_tex), 2);
            let u_prev_data_tex = uniform("u_prev_data_tex")?;
            gl.uniform_1_i32(Some(&u_prev_data_tex), 3);

            let u_radar_center = uniform("u_radar_center")?;
            let u_radar_radius = uniform("u_radar_radius")?;
            let u_gate_count = uniform("u_gate_count")?;
            let u_azimuth_count = uniform("u_azimuth_count")?;
            let u_first_gate_km = uniform("u_first_gate_km")?;
            let u_gate_interval_km = uniform("u_gate_interval_km")?;
            let u_max_range_km = uniform("u_max_range_km")?;
            let u_value_min = uniform("u_value_min")?;
            let u_value_range = uniform("u_value_range")?;
            let u_viewport_size = uniform("u_viewport_size")?;

            // Processing uniforms
            let u_interpolation = uniform("u_interpolation")?;
            let u_smoothing_enabled = uniform("u_smoothing_enabled")?;
            let u_smoothing_radius = uniform("u_smoothing_radius")?;
            let u_despeckle_enabled = uniform("u_despeckle_enabled")?;
            let u_despeckle_threshold = uniform("u_despeckle_threshold")?;
            let u_opacity = uniform("u_opacity")?;
            let u_edge_softening = uniform("u_edge_softening")?;

            // Raw-to-physical conversion uniforms
            let u_offset = uniform("u_offset")?;
            let u_scale = uniform("u_scale")?;

            // Sweep animation uniforms
            let u_sweep_enabled = uniform("u_sweep_enabled")?;
            let u_sweep_azimuth = uniform("u_sweep_azimuth")?;
            let u_sweep_start = uniform("u_sweep_start")?;
            let u_prev_offset = uniform("u_prev_offset")?;
            let u_prev_scale = uniform("u_prev_scale")?;
            let u_prev_gate_count = uniform("u_prev_gate_count")?;
            let u_prev_azimuth_count = uniform("u_prev_azimuth_count")?;
            let u_prev_first_gate_km = uniform("u_prev_first_gate_km")?;
            let u_prev_gate_interval_km = uniform("u_prev_gate_interval_km")?;
            let u_prev_max_range_km = uniform("u_prev_max_range_km")?;

            // Create placeholder for previous data texture
            let prev_data_texture = create_r32f_texture(gl, 1, 1, &[0.0]);

            gl.use_program(None);

            Ok(Self {
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
                u_interpolation,
                u_smoothing_enabled,
                u_smoothing_radius,
                u_despeckle_enabled,
                u_despeckle_threshold,
                u_opacity,
                u_edge_softening,
                u_offset,
                u_scale,
                u_sweep_enabled,
                u_sweep_azimuth,
                u_sweep_start,
                u_prev_offset,
                u_prev_scale,
                u_prev_gate_count,
                u_prev_azimuth_count,
                u_prev_first_gate_km,
                u_prev_gate_interval_km,
                u_prev_max_range_km,
                prev_data_texture,
                prev_data_offset: 0.0,
                prev_data_scale: 1.0,
                prev_azimuth_count: 0,
                prev_gate_count: 0,
                prev_first_gate_km: 0.0,
                prev_gate_interval_km: 0.0,
                prev_max_range_km: 0.0,
                prev_sweep_id: None,
                azimuth_count: 0,
                gate_count: 0,
                first_gate_km: 0.0,
                gate_interval_km: 0.0,
                max_range_km: 0.0,
                value_min: 0.0,
                value_range: 1.0,
                has_data: false,
                data_offset: 0.0,
                data_scale: 1.0,
                current_sweep_id: None,
                cpu_azimuths: Vec::new(),
                cpu_gate_values: Vec::new(),
                cpu_radial_times: Vec::new(),
            })
        }
    }

    /// Upload decoded radar data to GPU textures.
    ///
    /// `gate_values` contains raw u16 values cast to f32.
    /// Sentinels: 0 = below threshold, 1 = range folded.
    /// Physical value = (raw - offset) / scale.
    #[allow(clippy::too_many_arguments)]
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
        offset: f32,
        scale: f32,
        radial_times: &[f64],
    ) {
        let t_total = web_time::Instant::now();

        self.azimuth_count = azimuth_count;
        self.gate_count = gate_count;
        self.first_gate_km = first_gate_km;
        self.gate_interval_km = gate_interval_km;
        self.max_range_km = max_range_km;
        self.data_offset = offset;
        self.data_scale = scale;
        self.has_data = azimuth_count > 0 && gate_count > 0;

        // Keep CPU copies for inspector value lookup
        let t_copy = web_time::Instant::now();
        self.cpu_azimuths = azimuths.to_vec();
        self.cpu_gate_values = gate_values.to_vec();
        self.cpu_radial_times = radial_times.to_vec();
        let copy_ms = t_copy.elapsed().as_secs_f64() * 1000.0;

        if !self.has_data {
            return;
        }

        let t_upload = web_time::Instant::now();
        unsafe {
            // Re-create data texture (gates x azimuths, R32F)
            gl.delete_texture(self.data_texture);
            self.data_texture =
                create_r32f_texture(gl, gate_count as i32, azimuth_count as i32, gate_values);

            // Re-create azimuth texture (Nx1, R32F)
            gl.delete_texture(self.azimuth_texture);
            self.azimuth_texture = create_r32f_texture(gl, azimuth_count as i32, 1, azimuths);
        }
        let upload_ms = t_upload.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "GPU update_data: {}x{} (az x gates), range {:.1}-{:.1} km, {:.1}ms (copy: {:.1}ms, upload: {:.1}ms)",
            azimuth_count,
            gate_count,
            first_gate_km,
            max_range_km,
            total_ms,
            copy_ms,
            upload_ms,
        );
    }

    /// Build and upload a color lookup table for the given product.
    pub fn update_color_table(&mut self, gl: &glow::Context, product_str: &str) {
        let t_total = web_time::Instant::now();

        let product = product_from_str(product_str);
        let (min_val, max_val) = product_value_range(product);
        self.value_min = min_val;
        self.value_range = max_val - min_val;

        let t_build = web_time::Instant::now();

        // Build 1024-entry RGBA LUT (continuous gradient + GL_LINEAR = zero visible quantization)
        let lut_size = 1024usize;
        let lut_data = if matches!(product, Product::Reflectivity) {
            // OKLab-interpolated reflectivity palette with alpha ramp
            build_reflectivity_lut(min_val, max_val)
        } else {
            let color_scale = continuous_color_scale(product);
            let mut data = Vec::with_capacity(lut_size * 4);
            for i in 0..lut_size {
                let t = i as f32 / (lut_size - 1) as f32;
                let value = min_val + t * (max_val - min_val);
                let color = color_scale.color(value);
                let rgba = color.to_rgba8();
                data.extend_from_slice(&rgba);
            }
            data
        };
        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

        let t_upload = web_time::Instant::now();
        unsafe {
            gl.delete_texture(self.lut_texture);
            self.lut_texture = create_rgba8_texture(gl, lut_size as i32, 1, &lut_data);
        }
        let upload_ms = t_upload.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "GPU update_color_table: {:?} ({:.1}..{:.1}), {:.1}ms (build: {:.1}ms, upload: {:.1}ms)",
            product,
            min_val,
            max_val,
            total_ms,
            build_ms,
            upload_ms,
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

    // --- Accessors for globe radar renderer ---

    pub fn gate_count(&self) -> u32 {
        self.gate_count
    }
    pub fn azimuth_count(&self) -> u32 {
        self.azimuth_count
    }
    pub fn first_gate_km(&self) -> f64 {
        self.first_gate_km
    }
    pub fn gate_interval_km(&self) -> f64 {
        self.gate_interval_km
    }
    pub fn value_min(&self) -> f32 {
        self.value_min
    }
    pub fn value_range(&self) -> f32 {
        self.value_range
    }
    pub fn data_offset(&self) -> f32 {
        self.data_offset
    }
    pub fn data_scale(&self) -> f32 {
        self.data_scale
    }
    pub fn data_texture(&self) -> glow::Texture {
        self.data_texture
    }
    pub fn lut_texture(&self) -> glow::Texture {
        self.lut_texture
    }
    pub fn azimuth_texture(&self) -> glow::Texture {
        self.azimuth_texture
    }

    /// Clear all radar data (e.g. on site change).
    pub fn clear_data(&mut self) {
        self.has_data = false;
        self.current_sweep_id = None;
        self.prev_sweep_id = None;
        self.cpu_azimuths.clear();
        self.cpu_gate_values.clear();
        self.cpu_radial_times.clear();
    }

    /// Identity of the sweep currently loaded in the primary data texture.
    pub fn current_sweep_id(&self) -> Option<&str> {
        self.current_sweep_id.as_deref()
    }

    /// Identity of the sweep currently loaded in the previous data texture.
    pub fn prev_sweep_id(&self) -> Option<&str> {
        self.prev_sweep_id.as_deref()
    }

    /// Set the identity of the current sweep (called after `update_data`).
    pub fn set_current_sweep_id(&mut self, id: Option<String>) {
        self.current_sweep_id = id;
    }

    /// Upload decoded radar data to the *previous* texture slot for sweep
    /// animation compositing. Stores per-sweep spatial metadata so the shader
    /// can sample the previous texture with correct gate/range mapping even
    /// when the current and previous sweeps have different dimensions.
    #[allow(clippy::too_many_arguments)]
    pub fn update_previous_data(
        &mut self,
        gl: &glow::Context,
        gate_values: &[f32],
        azimuth_count: u32,
        gate_count: u32,
        first_gate_km: f64,
        gate_interval_km: f64,
        max_range_km: f64,
        offset: f32,
        scale: f32,
        sweep_id: Option<String>,
    ) {
        self.prev_data_offset = offset;
        self.prev_data_scale = scale;
        self.prev_azimuth_count = azimuth_count;
        self.prev_gate_count = gate_count;
        self.prev_first_gate_km = first_gate_km;
        self.prev_gate_interval_km = gate_interval_km;
        self.prev_max_range_km = max_range_km;
        self.prev_sweep_id = sweep_id;

        if azimuth_count == 0 || gate_count == 0 {
            return;
        }

        unsafe {
            gl.delete_texture(self.prev_data_texture);
            self.prev_data_texture =
                create_r32f_texture(gl, gate_count as i32, azimuth_count as i32, gate_values);
        }
    }

    /// Look up the raw data value at a given polar coordinate.
    ///
    /// Returns `None` if outside the data range or if the value is the no-data sentinel.
    pub fn value_at_polar(&self, azimuth_deg: f32, range_km: f64) -> Option<f32> {
        if !self.has_data || self.cpu_azimuths.is_empty() {
            return None;
        }

        // Check range bounds
        if range_km < self.first_gate_km || range_km >= self.max_range_km {
            return None;
        }

        // Find nearest azimuth index
        let az_count = self.azimuth_count as usize;
        let gate_count = self.gate_count as usize;
        let mut best_idx = 0usize;
        let mut best_dist = 360.0f32;
        for (i, &az) in self.cpu_azimuths.iter().enumerate() {
            let mut d = (azimuth_deg - az).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            if d < best_dist {
                best_dist = d;
                best_idx = i;
            }
        }

        // Gap check: if nearest azimuth is too far away, no data
        let az_spacing = 360.0 / az_count as f32;
        if best_dist > az_spacing * 1.5 {
            return None;
        }

        // Compute gate index
        let gate_idx = ((range_km - self.first_gate_km) / self.gate_interval_km).floor() as usize;
        if gate_idx >= gate_count {
            return None;
        }

        // Data layout: row-major [azimuth][gate]
        let offset = best_idx * gate_count + gate_idx;
        if offset >= self.cpu_gate_values.len() {
            return None;
        }

        let raw = self.cpu_gate_values[offset];
        // Raw sentinels: 0 = below threshold, 1 = range folded
        if raw <= 1.0 {
            return None;
        }

        // Convert raw to physical value
        if self.data_scale == 0.0 {
            Some(raw)
        } else {
            Some((raw - self.data_offset) / self.data_scale)
        }
    }

    /// Look up the radial collection timestamp (Unix seconds) at a given azimuth.
    ///
    /// Returns `None` if radial times are not available or azimuth is out of range.
    pub fn collection_time_at_polar(&self, azimuth_deg: f32) -> Option<f64> {
        if self.cpu_radial_times.is_empty() || self.cpu_azimuths.is_empty() {
            return None;
        }

        let az_count = self.azimuth_count as usize;
        let mut best_idx = 0usize;
        let mut best_dist = 360.0f32;
        for (i, &az) in self.cpu_azimuths.iter().enumerate() {
            let mut d = (azimuth_deg - az).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            if d < best_dist {
                best_dist = d;
                best_idx = i;
            }
        }

        let az_spacing = 360.0 / az_count as f32;
        if best_dist > az_spacing * 1.5 {
            return None;
        }

        self.cpu_radial_times.get(best_idx).copied()
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
        processing: &RenderProcessing,
        sweep_info: Option<(f32, f32)>,
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
            gl.active_texture(glow::TEXTURE3);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.prev_data_texture));

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
            gl.uniform_2_f32(
                Some(&self.u_viewport_size),
                viewport_size[0],
                viewport_size[1],
            );

            // Processing uniforms
            let interp_mode = match processing.interpolation {
                crate::state::InterpolationMode::Nearest => 0,
                crate::state::InterpolationMode::Bilinear => 1,
            };
            gl.uniform_1_i32(Some(&self.u_interpolation), interp_mode);
            gl.uniform_1_i32(
                Some(&self.u_smoothing_enabled),
                processing.smoothing_enabled as i32,
            );
            gl.uniform_1_f32(Some(&self.u_smoothing_radius), processing.smoothing_radius);
            gl.uniform_1_i32(
                Some(&self.u_despeckle_enabled),
                processing.despeckle_enabled as i32,
            );
            gl.uniform_1_i32(
                Some(&self.u_despeckle_threshold),
                processing.despeckle_threshold as i32,
            );
            gl.uniform_1_f32(Some(&self.u_opacity), processing.opacity);
            gl.uniform_1_i32(
                Some(&self.u_edge_softening),
                processing.edge_softening as i32,
            );

            // Raw-to-physical conversion
            gl.uniform_1_f32(Some(&self.u_offset), self.data_offset);
            gl.uniform_1_f32(Some(&self.u_scale), self.data_scale);

            // Sweep animation uniforms — enable even without prev data; the 1x1
            // placeholder texture returns 0.0 (below-threshold sentinel → transparent),
            // so the first sweep progressively reveals against a blank background.
            let sweep_on = sweep_info.is_some();
            gl.uniform_1_i32(Some(&self.u_sweep_enabled), sweep_on as i32);
            let (sweep_az, sweep_start) = sweep_info.unwrap_or((0.0, 0.0));
            gl.uniform_1_f32(Some(&self.u_sweep_azimuth), sweep_az);
            gl.uniform_1_f32(Some(&self.u_sweep_start), sweep_start);
            gl.uniform_1_f32(Some(&self.u_prev_offset), self.prev_data_offset);
            gl.uniform_1_f32(Some(&self.u_prev_scale), self.prev_data_scale);
            gl.uniform_1_f32(Some(&self.u_prev_gate_count), self.prev_gate_count as f32);
            gl.uniform_1_f32(
                Some(&self.u_prev_azimuth_count),
                self.prev_azimuth_count as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_prev_first_gate_km),
                self.prev_first_gate_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_prev_gate_interval_km),
                self.prev_gate_interval_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.u_prev_max_range_km),
                self.prev_max_range_km as f32,
            );

            // Draw fullscreen quad
            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            // Unbind our resources so we don't interfere with egui
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
        }
    }

    /// Detect storm cells from the current CPU-side data.
    ///
    /// Returns lightweight cell info for rendering on the canvas.
    /// Uses nexrad-process connected-component analysis on the reflectivity data.
    pub fn detect_storm_cells(
        &self,
        radar_lat: f64,
        radar_lon: f64,
        threshold_dbz: f32,
    ) -> Vec<crate::state::StormCellInfo> {
        if !self.has_data || self.cpu_azimuths.is_empty() {
            return Vec::new();
        }

        let t_total = web_time::Instant::now();

        let az_count = self.azimuth_count as usize;
        let gate_count = self.gate_count as usize;

        // Compute azimuth spacing
        let az_spacing = if az_count > 1 {
            360.0 / az_count as f32
        } else {
            1.0
        };

        // Build a SweepField from the CPU data
        let t_field = web_time::Instant::now();
        let mut field = nexrad_model::data::SweepField::new_empty(
            "Reflectivity",
            "dBZ",
            0.5, // elevation doesn't matter for 2D detection
            self.cpu_azimuths.clone(),
            az_spacing,
            self.first_gate_km,
            self.gate_interval_km,
            gate_count,
        );

        // Populate the field with our gate values (convert raw → physical)
        let mut valid_gates = 0u32;
        for az_idx in 0..az_count {
            let row_start = az_idx * gate_count;
            for g in 0..gate_count {
                let raw = self.cpu_gate_values[row_start + g];
                // Raw sentinels: 0 = below threshold, 1 = range folded
                if raw > 1.0 {
                    let physical = if self.data_scale == 0.0 {
                        raw
                    } else {
                        (raw - self.data_offset) / self.data_scale
                    };
                    field.set(az_idx, g, physical, nexrad_model::data::GateStatus::Valid);
                    valid_gates += 1;
                }
                // new_empty defaults to NoData, so we only set Valid gates
            }
        }
        let field_ms = t_field.elapsed().as_secs_f64() * 1000.0;

        // Build coordinate system from site location
        use nexrad_model::geo::RadarCoordinateSystem;
        use nexrad_model::meta::Site;
        use nexrad_process::detection::StormCellDetector;

        let site = Site::new(
            *b"SITE",
            radar_lat as f32,
            radar_lon as f32,
            0, // altitude (not critical for 2D detection)
            0, // tower height
        );
        let coord_system = RadarCoordinateSystem::new(&site);

        // Run detection
        let t_detect = web_time::Instant::now();
        let detector: StormCellDetector = match StormCellDetector::new(threshold_dbz, 10) {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };

        let cells: Vec<nexrad_process::detection::StormCell> =
            match detector.detect(&field, &coord_system) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("Storm cell detection failed: {}", e);
                    return Vec::new();
                }
            };
        let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;

        // Convert to lightweight info, filtering out small noise cells
        let t_convert = web_time::Instant::now();
        const MIN_AREA_KM2: f64 = 5.0;

        let result: Vec<_> = cells
            .iter()
            .filter(|cell| cell.area_km2() >= MIN_AREA_KM2)
            .map(|cell| {
                let centroid = cell.centroid();
                let bounds = cell.bounds();
                crate::state::StormCellInfo {
                    lat: centroid.latitude,
                    lon: centroid.longitude,
                    max_dbz: cell.max_reflectivity_dbz(),
                    area_km2: cell.area_km2() as f32,
                    bounds: (
                        bounds.min_latitude(),
                        bounds.min_longitude(),
                        bounds.max_latitude(),
                        bounds.max_longitude(),
                    ),
                }
            })
            .collect();
        let convert_ms = t_convert.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "detect_storm_cells: {}x{} grid, {} valid gates, {} raw cells, {} after filter (>={:.0} km2), {:.1}ms (field: {:.1}ms, detect: {:.1}ms, convert: {:.1}ms)",
            az_count,
            gate_count,
            valid_gates,
            cells.len(),
            result.len(),
            MIN_AREA_KM2,
            total_ms,
            field_ms,
            detect_ms,
            convert_ms,
        );

        result
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Cast an `&[f32]` to `&[u8]` for GL upload.
fn bytemuck_cast_slice(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
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
