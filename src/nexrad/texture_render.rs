//! NEXRAD rendering using the nexrad-render crate.
//!
//! This module provides high-performance radar rendering using the nexrad-render
//! crate. The rendered image is cached as a texture for efficient display.

use eframe::egui::ColorImage;
use nexrad::prelude::{Radial, Volume};
use nexrad_render::{get_nws_reflectivity_scale, render_radials, Product, RenderOptions};

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
#[allow(dead_code)]
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

    // Render using nexrad-render
    let options = RenderOptions::new(dimensions.0, dimensions.1).transparent();
    let image = render_radials(
        radials,
        Product::Reflectivity,
        &get_nws_reflectivity_scale(),
        &options,
    )
    .map_err(|e| format!("Failed to render radials: {}", e))?;

    // Convert RgbaImage to egui ColorImage
    let (width, height) = image.dimensions();
    let pixels = image.into_raw();

    Ok(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &pixels,
    ))
}

/// Result of rendering radials, including timing information.
pub struct RenderResult {
    /// The rendered image
    pub image: ColorImage,
    /// Time taken to render in milliseconds
    pub render_time_ms: f64,
}

/// Renders a collection of radials to an egui ColorImage.
///
/// This function is used for dynamic sweep rendering where radials may come
/// from multiple volumes. Unlike `render_sweep_to_image`, this takes a pre-built
/// collection of radial references rather than extracting them from a volume.
///
/// Note: This function clones radials into a contiguous buffer because the
/// nexrad-render crate requires `&[Radial]`. This is necessary when radials
/// come from different volumes in memory.
///
/// # Arguments
/// * `radials` - Slice of radial references to render (should be sorted by azimuth)
/// * `dimensions` - Output image dimensions (width, height)
///
/// # Returns
/// A `RenderResult` containing the image and timing info, or an error message.
pub fn render_radials_to_image(
    radials: &[&Radial],
    dimensions: (usize, usize),
) -> Result<RenderResult, String> {
    let start = web_time::Instant::now();

    if radials.is_empty() {
        return Err("No radials to render".to_string());
    }

    // Clone radials into a contiguous Vec (required by render_radials)
    let owned_radials: Vec<Radial> = radials.iter().map(|r| (*r).clone()).collect();

    // Render using nexrad-render
    let options = RenderOptions::new(dimensions.0, dimensions.1).transparent();
    let image = render_radials(
        &owned_radials,
        Product::Reflectivity,
        &get_nws_reflectivity_scale(),
        &options,
    )
    .map_err(|e| format!("Failed to render radials: {}", e))?;

    // Convert RgbaImage to egui ColorImage
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
