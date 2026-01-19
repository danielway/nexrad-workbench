//! Map projection and coordinate transformation.
//!
//! Handles converting between geographic coordinates (lat/lon) and
//! screen coordinates for rendering on the canvas.

use eframe::egui::{Pos2, Rect, Vec2};
use geo_types::Coord;

/// Map projection for converting geographic to screen coordinates.
#[derive(Debug, Clone)]
pub struct MapProjection {
    /// Center latitude of the view (radar site location)
    pub center_lat: f64,
    /// Center longitude of the view (radar site location)
    pub center_lon: f64,
    /// Visible range in degrees (how much lat/lon span is visible)
    pub range_deg: f64,
    /// Current zoom level
    pub zoom: f32,
    /// Pan offset in screen pixels
    pub pan_offset: Vec2,
    /// Screen rectangle for the canvas
    pub screen_rect: Rect,
}

impl Default for MapProjection {
    fn default() -> Self {
        Self {
            // Default to center of continental US
            center_lat: 39.0,
            center_lon: -98.0,
            // ~500km radius view (~4.5 degrees)
            range_deg: 4.5,
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            screen_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        }
    }
}

impl MapProjection {
    /// Creates a new projection centered on a radar site.
    pub fn new(center_lat: f64, center_lon: f64) -> Self {
        Self {
            center_lat,
            center_lon,
            ..Default::default()
        }
    }

    /// Updates the projection with current view state.
    pub fn update(&mut self, zoom: f32, pan_offset: Vec2, screen_rect: Rect) {
        self.zoom = zoom;
        self.pan_offset = pan_offset;
        self.screen_rect = screen_rect;
    }

    /// Converts geographic coordinates (lon, lat) to screen position.
    ///
    /// Uses a simple equirectangular projection which is adequate for
    /// the typical ~500km range of NEXRAD displays.
    pub fn geo_to_screen(&self, coord: Coord<f64>) -> Pos2 {
        let lon = coord.x;
        let lat = coord.y;

        // Calculate the effective range based on zoom
        let effective_range = self.range_deg / self.zoom as f64;

        // Normalize coordinates relative to center
        let rel_lon = lon - self.center_lon;
        let rel_lat = lat - self.center_lat;

        // Apply latitude correction for longitude (approximate Mercator-like behavior)
        let lat_correction = (self.center_lat.to_radians()).cos();
        let corrected_lon = rel_lon * lat_correction;

        // Convert to normalized coordinates (-1 to 1)
        let norm_x = corrected_lon / effective_range;
        let norm_y = -rel_lat / effective_range; // Flip Y since screen Y increases downward

        // Convert to screen coordinates
        let center = self.screen_rect.center() + self.pan_offset;
        let half_size = self.screen_rect.size().min_elem() / 2.0;

        Pos2::new(
            center.x + (norm_x as f32) * half_size,
            center.y + (norm_y as f32) * half_size,
        )
    }

    /// Converts screen position to geographic coordinates (lon, lat).
    pub fn screen_to_geo(&self, pos: Pos2) -> Coord<f64> {
        let effective_range = self.range_deg / self.zoom as f64;

        let center = self.screen_rect.center() + self.pan_offset;
        let half_size = self.screen_rect.size().min_elem() / 2.0;

        // Convert from screen to normalized
        let norm_x = (pos.x - center.x) / half_size;
        let norm_y = (pos.y - center.y) / half_size;

        // Convert from normalized to geographic
        let lat_correction = (self.center_lat.to_radians()).cos();
        let rel_lon = (norm_x as f64) * effective_range / lat_correction;
        let rel_lat = -(norm_y as f64) * effective_range; // Flip Y back

        Coord {
            x: self.center_lon + rel_lon,
            y: self.center_lat + rel_lat,
        }
    }

    /// Returns the visible geographic bounds as (min_lon, min_lat, max_lon, max_lat).
    pub fn visible_bounds(&self) -> (f64, f64, f64, f64) {
        let top_left = self.screen_to_geo(self.screen_rect.left_top());
        let bottom_right = self.screen_to_geo(self.screen_rect.right_bottom());

        (
            top_left.x.min(bottom_right.x),
            top_left.y.min(bottom_right.y),
            top_left.x.max(bottom_right.x),
            top_left.y.max(bottom_right.y),
        )
    }

    /// Checks if a coordinate is within the visible bounds (with margin).
    pub fn is_visible(&self, coord: Coord<f64>, margin_deg: f64) -> bool {
        let (min_lon, min_lat, max_lon, max_lat) = self.visible_bounds();
        coord.x >= min_lon - margin_deg
            && coord.x <= max_lon + margin_deg
            && coord.y >= min_lat - margin_deg
            && coord.y <= max_lat + margin_deg
    }

    /// Checks if a bounding box intersects with the visible bounds.
    pub fn bbox_visible(&self, min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> bool {
        let (vis_min_lon, vis_min_lat, vis_max_lon, vis_max_lat) = self.visible_bounds();

        // Add margin for edge cases
        let margin = 1.0;
        !(max_lon < vis_min_lon - margin
            || min_lon > vis_max_lon + margin
            || max_lat < vis_min_lat - margin
            || min_lat > vis_max_lat + margin)
    }
}
