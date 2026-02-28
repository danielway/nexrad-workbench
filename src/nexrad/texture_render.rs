//! NEXRAD rendering using the nexrad-render crate.
//!
//! This module provides high-performance radar rendering using the nexrad-render
//! crate. The rendered image is cached as a texture for efficient display.
//! All rendering goes through the SweepField API at native resolution.

use ::nexrad::model::data::SweepField;
use eframe::egui::ColorImage;
use nexrad_render::{default_color_scale, render_sweep, Interpolation, Product, RenderOptions};

/// Result of rendering, including timing information.
pub struct RenderResult {
    /// The rendered image
    pub image: ColorImage,
    /// Time taken to render in milliseconds
    pub render_time_ms: f64,
}

/// Renders a SweepField to an egui ColorImage at native resolution.
///
/// Uses `RenderOptions::native_for` to automatically size the output to
/// `gate_count * 2` in each dimension, providing approximately one pixel
/// per gate at the outer edge of the sweep.
pub fn render_sweep_field_to_image(
    field: &SweepField,
    product: Product,
    interpolation: Interpolation,
) -> Result<RenderResult, String> {
    let start = web_time::Instant::now();

    let color_scale = default_color_scale(product);
    let options = RenderOptions::native_for(field)
        .transparent()
        .with_interpolation(interpolation);

    let result = render_sweep(field, &color_scale, &options)
        .map_err(|e| format!("Failed to render sweep field: {}", e))?;

    let image = result.into_image();
    let (width, height) = image.dimensions();
    let pixels = image.into_raw();
    let egui_image = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &pixels);

    let render_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(RenderResult {
        image: egui_image,
        render_time_ms,
    })
}

/// Returns the standard NEXRAD coverage range in km.
///
/// Standard NEXRAD range is approximately 230km for base reflectivity
/// and up to 460km for long-range products.
pub fn radar_coverage_range_km() -> f64 {
    300.0
}
