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
}

impl RadarCacheKey {
    /// Create a cache key from a data ID string (for static rendering).
    ///
    /// This maintains backward compatibility with the original API.
    #[allow(dead_code)]
    pub fn new(data_id: String, sweep_index: usize, dimensions: (usize, usize)) -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        data_id.hash(&mut hasher);
        let content_signature = hasher.finish();

        Self {
            content_signature,
            elevation_index: sweep_index,
            dimensions,
        }
    }

    /// Create a cache key for a dynamic sweep render.
    ///
    /// Uses a pre-computed content signature from RenderSweep::cache_signature().
    pub fn for_dynamic_sweep(
        content_signature: u64,
        elevation_index: usize,
        dimensions: (usize, usize),
    ) -> Self {
        Self {
            content_signature,
            elevation_index,
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
