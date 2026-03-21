//! Color table and LUT generation for radar products.
//!
//! Pure functions for building color lookup tables (no GL dependency).
//! Used by `gpu_renderer` for texture uploads and by `canvas` for legend rendering.

use nexrad_render::{Color as NrColor, ColorScale, ColorStop, ContinuousColorScale, Product};

/// Default value ranges per product (used for color LUT normalization).
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

pub fn product_from_str(s: &str) -> Product {
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
