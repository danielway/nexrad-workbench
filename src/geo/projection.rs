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

    /// Sets the center coordinates (e.g., when radar site changes).
    pub fn set_center(&mut self, lat: f64, lon: f64) {
        self.center_lat = lat;
        self.center_lon = lon;
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

/// Lookup table for common NEXRAD radar site locations.
/// Format: (site_id, latitude, longitude)
pub const NEXRAD_SITES: &[(&str, f64, f64)] = &[
    ("KTLX", 35.3331, -97.2778),  // Oklahoma City, OK
    ("KFWS", 32.5730, -97.3031),  // Dallas/Fort Worth, TX
    ("KEWX", 29.7039, -98.0286),  // Austin/San Antonio, TX
    ("KFDR", 34.3622, -98.9764),  // Frederick, OK
    ("KINX", 36.1750, -95.5647),  // Tulsa, OK
    ("KVNX", 36.7408, -98.1275),  // Vance AFB, OK
    ("KDYX", 32.5381, -99.2542),  // Dyess AFB, TX
    ("KAMA", 35.2331, -101.7092), // Amarillo, TX
    ("KLBB", 33.6542, -101.8142), // Lubbock, TX
    ("KMAF", 31.9433, -102.1892), // Midland, TX
    ("KSJT", 31.3711, -100.4925), // San Angelo, TX
    ("KGRK", 30.7217, -97.3828),  // Fort Hood, TX
    ("KHGX", 29.4719, -95.0792),  // Houston, TX
    ("KCRP", 27.7842, -97.5111),  // Corpus Christi, TX
    ("KBRO", 25.9158, -97.4189),  // Brownsville, TX
    ("KLZK", 34.8364, -92.2622),  // Little Rock, AR
    ("KSRX", 35.2906, -94.3617),  // Fort Smith, AR
    ("KSHV", 32.4508, -93.8414),  // Shreveport, LA
    ("KLCH", 30.1253, -93.2161),  // Lake Charles, LA
    ("KPOE", 31.1556, -92.9758),  // Fort Polk, LA
    ("KLIX", 30.3367, -89.8256),  // New Orleans, LA
    ("KDGX", 32.2797, -89.9844),  // Jackson, MS
    ("KGWX", 33.8967, -88.3292),  // Columbus AFB, MS
    ("KMOB", 30.6794, -88.2397),  // Mobile, AL
    ("KBMX", 33.1722, -86.7697),  // Birmingham, AL
    ("KEOX", 31.4606, -85.4594),  // Fort Rucker, AL
    ("KMXX", 32.5369, -85.7897),  // Maxwell AFB, AL
    ("KHTX", 34.9306, -86.0833),  // Huntsville, AL
    ("KJGX", 32.6753, -83.3511),  // Robins AFB, GA
    ("KFFC", 33.3636, -84.5658),  // Atlanta, GA
    ("KVAX", 30.8903, -83.0019),  // Moody AFB, GA
    ("KJAX", 30.4847, -81.7019),  // Jacksonville, FL
    ("KTLH", 30.3975, -84.3289),  // Tallahassee, FL
    ("KEVX", 30.5644, -85.9214),  // Eglin AFB, FL
    ("KMLB", 28.1131, -80.6542),  // Melbourne, FL
    ("KTBW", 27.7056, -82.4017),  // Tampa Bay, FL
    ("KAMX", 25.6111, -80.4128),  // Miami, FL
    ("KBYX", 24.5975, -81.7031),  // Key West, FL
                                  // Add more sites as needed...
];

/// Looks up radar site coordinates by ID.
pub fn lookup_site(site_id: &str) -> Option<(f64, f64)> {
    let site_upper = site_id.to_uppercase();
    NEXRAD_SITES
        .iter()
        .find(|(id, _, _)| *id == site_upper)
        .map(|(_, lat, lon)| (*lat, *lon))
}
