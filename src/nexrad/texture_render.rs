//! NEXRAD rendering using the nexrad-render crate.
//!
//! This module provides high-performance radar rendering using the nexrad-render
//! crate with piet-common backend. On native builds this uses Cairo, on WASM it
//! uses piet-web. The rendered image is cached as a texture for efficient display.

use eframe::egui::ColorImage;
use nexrad::prelude::Volume;
use nexrad::render::{get_nws_reflectivity_scale, render_radials, Product};
use piet_common::Device;

/// Renders a sweep from a NEXRAD volume to an egui ColorImage.
///
/// This function uses the nexrad-render crate to render radar data to an image,
/// then converts it to an egui-compatible format for texture caching.
///
/// # Arguments
/// * `volume` - The NEXRAD volume data containing sweeps
/// * `sweep_index` - Index of the sweep to render (0 = lowest elevation)
/// * `dimensions` - Output image dimensions (width, height)
///
/// # Returns
/// A `ColorImage` ready to be loaded as an egui texture, or an error message.
pub fn render_sweep_to_image(
    volume: &Volume,
    sweep_index: usize,
    dimensions: (usize, usize),
) -> Result<ColorImage, String> {
    // Get the requested sweep
    let sweeps: Vec<_> = volume.sweeps().iter().collect();
    let sweep = sweeps.get(sweep_index).ok_or_else(|| {
        format!(
            "Sweep index {} out of range (total: {})",
            sweep_index,
            sweeps.len()
        )
    })?;

    // Get radials for rendering
    let radials = sweep.radials();
    if radials.is_empty() {
        return Err("No radials in sweep".to_string());
    }

    // Create piet device for rendering
    let mut device = Device::new().map_err(|e| format!("Failed to create render device: {}", e))?;

    // Render using nexrad-render
    let mut target = render_radials(
        &mut device,
        radials,
        Product::Reflectivity,
        &get_nws_reflectivity_scale(),
        dimensions,
    )
    .map_err(|e| format!("Failed to render radials: {}", e))?;

    // Extract pixel data from the render target
    let (width, height) = dimensions;
    let buffer_size = width * height * 4; // RGBA = 4 bytes per pixel
    let mut buffer = vec![0u8; buffer_size];

    target
        .copy_raw_pixels(piet_common::ImageFormat::RgbaPremul, &mut buffer)
        .map_err(|e| format!("Failed to copy pixel data: {}", e))?;

    // Convert from premultiplied alpha to straight alpha for egui
    // egui expects non-premultiplied RGBA
    for pixel in buffer.chunks_exact_mut(4) {
        let a = pixel[3] as f32 / 255.0;
        if a > 0.0 {
            pixel[0] = (pixel[0] as f32 / a).min(255.0) as u8;
            pixel[1] = (pixel[1] as f32 / a).min(255.0) as u8;
            pixel[2] = (pixel[2] as f32 / a).min(255.0) as u8;
        }
    }

    // Create egui ColorImage from the RGBA buffer
    Ok(ColorImage::from_rgba_unmultiplied([width, height], &buffer))
}

/// Returns the standard NEXRAD coverage range in km.
///
/// Standard NEXRAD range is approximately 230km for base reflectivity
/// and up to 460km for long-range products.
pub fn radar_coverage_range_km() -> f64 {
    300.0
}
