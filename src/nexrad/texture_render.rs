//! NEXRAD rendering using the nexrad-render crate.
//!
//! This module provides high-performance radar rendering using the nexrad-render
//! crate. The rendered image is cached as a texture for efficient display.
//! Supports product selection, interpolation modes, and the new SweepField API.

use ::nexrad::model::data::{Radial, Scan, SweepField};
use eframe::egui::ColorImage;
use nexrad_render::{
    default_color_scale, default_scale, render_radials, render_sweep, Interpolation, Product,
    RenderOptions,
};

/// Result of rendering radials, including timing information.
pub struct RenderResult {
    /// The rendered image
    pub image: ColorImage,
    /// Time taken to render in milliseconds
    pub render_time_ms: f64,
}

/// Renders a sweep from a NEXRAD scan to an egui ColorImage.
///
/// # Arguments
/// * `scan` - The NEXRAD scan data containing sweeps
/// * `sweep_index` - Index of the sweep to render (0 = lowest elevation)
/// * `product` - The radar product to render
/// * `interpolation` - Interpolation mode (nearest or bilinear)
/// * `dimensions` - Output image dimensions (width, height)
///
/// # Returns
/// A `ColorImage` ready to be loaded as an egui texture, or an error message.
#[allow(dead_code)]
pub fn render_sweep_to_image(
    scan: &Scan,
    sweep_index: usize,
    product: Product,
    interpolation: Interpolation,
    dimensions: (usize, usize),
) -> Result<ColorImage, String> {
    let sweeps: Vec<_> = scan.sweeps().iter().collect();
    let sweep = sweeps.get(sweep_index).ok_or_else(|| {
        format!(
            "Sweep index {} out of range (total: {})",
            sweep_index,
            sweeps.len()
        )
    })?;

    let radials = sweep.radials();
    if radials.is_empty() {
        return Err("No radials in sweep".to_string());
    }

    let scale = default_scale(product);
    let options = RenderOptions::new(dimensions.0, dimensions.1)
        .transparent()
        .with_interpolation(interpolation);
    let image = render_radials(radials, product, &scale, &options)
        .map_err(|e| format!("Failed to render radials: {}", e))?;

    let (width, height) = image.dimensions();
    let pixels = image.into_raw();

    Ok(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &pixels,
    ))
}

/// Renders a collection of radials to an egui ColorImage.
///
/// This function is used for dynamic sweep rendering where radials may come
/// from multiple volumes. Unlike `render_sweep_to_image`, this takes a pre-built
/// collection of radial references rather than extracting them from a scan.
///
/// Note: This function clones radials into a contiguous buffer because the
/// nexrad-render crate requires `&[Radial]`. This is necessary when radials
/// come from different scans in memory.
///
/// # Arguments
/// * `radials` - Slice of radial references to render (should be sorted by azimuth)
/// * `product` - The radar product to render
/// * `interpolation` - Interpolation mode (nearest or bilinear)
/// * `dimensions` - Output image dimensions (width, height)
///
/// # Returns
/// A `RenderResult` containing the image and timing info, or an error message.
pub fn render_radials_to_image(
    radials: &[&Radial],
    product: Product,
    interpolation: Interpolation,
    dimensions: (usize, usize),
) -> Result<RenderResult, String> {
    let start = web_time::Instant::now();

    if radials.is_empty() {
        return Err("No radials to render".to_string());
    }

    // Clone radials into a contiguous Vec (required by render_radials)
    let owned_radials: Vec<Radial> = radials.iter().map(|r| (*r).clone()).collect();

    let scale = default_scale(product);
    let options = RenderOptions::new(dimensions.0, dimensions.1)
        .transparent()
        .with_interpolation(interpolation);
    let image = render_radials(&owned_radials, product, &scale, &options)
        .map_err(|e| format!("Failed to render radials: {}", e))?;

    let (width, height) = image.dimensions();
    let pixels = image.into_raw();
    let egui_image = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &pixels);

    let render_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(RenderResult {
        image: egui_image,
        render_time_ms,
    })
}

/// Renders a processed SweepField to an egui ColorImage.
///
/// This is the processing-pipeline-aware rendering path. It takes a pre-processed
/// SweepField (after filters/smoothing have been applied) and renders it using
/// `nexrad_render::render_sweep()`.
pub fn render_sweep_field_to_image(
    field: &SweepField,
    product: Product,
    interpolation: Interpolation,
    dimensions: (usize, usize),
) -> Result<RenderResult, String> {
    let start = web_time::Instant::now();

    let color_scale = default_color_scale(product);
    let options = RenderOptions::new(dimensions.0, dimensions.1)
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
