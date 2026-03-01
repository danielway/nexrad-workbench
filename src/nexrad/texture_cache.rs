//! Texture cache for rendered NEXRAD radar imagery.
//!
//! This module provides caching of rendered radar images as egui textures,
//! allowing efficient per-frame rendering without re-rendering the radar data.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Cache key for identifying radar texture state.
///
/// Supports two modes:
/// - Static: Uses a data_id string (for single-volume rendering)
/// - Dynamic: Uses a content signature hash (for multi-volume sweep rendering)
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RadarCacheKey {
    /// Content signature - hash of the radials included in the render.
    /// For static rendering, this is computed from data_id.
    /// For dynamic rendering, this comes from RenderSweep::cache_signature().
    pub content_signature: u64,
    /// Current sweep/elevation index
    pub elevation_index: usize,
    /// Rendered image dimensions
    pub dimensions: (usize, usize),
    /// Product discriminant (index into Product enum variants)
    pub product_index: u8,
    /// Interpolation mode (0=Nearest, 1=Bilinear)
    pub interpolation_mode: u8,
    /// Processing config hash (0 = no processing)
    pub processing_hash: u64,
}

impl RadarCacheKey {
    /// Create a cache key from a data ID string (for static rendering).
    ///
    /// This maintains backward compatibility with the original API.
    /// Create a cache key for a dynamic sweep render.
    ///
    /// Uses a pre-computed content signature from RenderSweep::cache_signature().
    /// Includes product and interpolation mode so the texture is re-rendered
    /// when these settings change.
    pub fn for_dynamic_sweep(
        content_signature: u64,
        elevation_index: usize,
        dimensions: (usize, usize),
    ) -> Self {
        Self {
            content_signature,
            elevation_index,
            dimensions,
            product_index: 0,
            interpolation_mode: 0,
            processing_hash: 0,
        }
    }
}

/// Texture cache for radar imagery.
///
/// Stores a rendered radar image as an egui texture, along with the cache key
/// used to determine if the cached texture is still valid.
pub struct RadarTextureCache {
    /// The cached texture handle
    texture: Option<TextureHandle>,
    /// The cache key for the current texture
    cache_key: Option<RadarCacheKey>,
}

impl Default for RadarTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RadarTextureCache {
    pub fn new() -> Self {
        Self {
            texture: None,
            cache_key: None,
        }
    }

    /// Update the cache with a new rendered image.
    pub fn update(&mut self, ctx: &egui::Context, key: RadarCacheKey, image: ColorImage) {
        log::debug!(
            "Updating radar texture cache: {}x{} for signature {}",
            image.width(),
            image.height(),
            key.content_signature
        );

        let texture = ctx.load_texture(
            "radar_texture",
            image,
            TextureOptions {
                magnification: egui::TextureFilter::Linear,
                minification: egui::TextureFilter::Linear,
                ..Default::default()
            },
        );

        self.texture = Some(texture);
        self.cache_key = Some(key);
    }

    /// Get the cached texture if available.
    pub fn texture(&self) -> Option<&TextureHandle> {
        self.texture.as_ref()
    }

    /// Invalidate the cache, forcing a re-render on next frame.
    pub fn invalidate(&mut self) {
        log::debug!("Invalidating radar texture cache");
        self.texture = None;
        self.cache_key = None;
    }
}
