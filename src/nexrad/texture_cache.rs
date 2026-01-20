//! Texture cache for rendered NEXRAD radar imagery.
//!
//! This module provides caching of rendered radar images as egui textures,
//! allowing efficient per-frame rendering without re-rendering the radar data.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Cache key for identifying radar texture state.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RadarCacheKey {
    /// Unique identifier for the scan data (e.g., filename or scan timestamp)
    pub data_id: String,
    /// Current sweep/elevation index
    pub sweep_index: usize,
    /// Rendered image dimensions
    pub dimensions: (usize, usize),
}

impl RadarCacheKey {
    pub fn new(data_id: String, sweep_index: usize, dimensions: (usize, usize)) -> Self {
        Self {
            data_id,
            sweep_index,
            dimensions,
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

    /// Check if the cache contains a valid texture for the given key.
    pub fn is_valid(&self, key: &RadarCacheKey) -> bool {
        self.cache_key.as_ref() == Some(key) && self.texture.is_some()
    }

    /// Update the cache with a new rendered image.
    pub fn update(&mut self, ctx: &egui::Context, key: RadarCacheKey, image: ColorImage) {
        log::debug!(
            "Updating radar texture cache: {}x{} for {:?}",
            image.width(),
            image.height(),
            key.data_id
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
