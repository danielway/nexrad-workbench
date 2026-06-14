//! Map projection and coordinate transformation.
//!
//! Handles converting between geographic coordinates (lat/lon) and
//! screen coordinates for rendering on the canvas.
//!
//! The [`Projection`] trait abstracts over both the 2D
//! [`MapProjection`] and a 3D wrapper around the
//! [`Camera`](crate::geo::camera::Camera) state machine. Both return
//! `Option<Pos2>` from `geo_to_screen` so the 3D side can signal
//! "behind the globe" — for 2D it's always `Some`.

use eframe::egui::{Pos2, Rect, Vec2};
use geo_types::Coord;

/// Unified geographic ↔ screen projection.
///
/// Implemented by [`MapProjection`] (2D, always projects) and by the
/// `GlobeProjection` wrapper in [`crate::geo::camera`] (3D, returns
/// `None` for points hidden behind the globe). UI code that doesn't
/// care which view mode is active can hold `&dyn Projection`.
pub trait Projection {
    /// Project a geographic coordinate to screen space, or `None` if
    /// the point isn't visible (e.g. behind a 3D globe).
    fn geo_to_screen(&self, lat: f64, lon: f64) -> Option<Pos2>;

    /// Project a screen-space position back to geographic coordinates,
    /// or `None` if the click missed the globe surface (3D only).
    fn screen_to_geo(&self, pos: Pos2) -> Option<(f64, f64)>;

    /// Axis-aligned geographic bounds currently in view, as
    /// `(min_lon, min_lat, max_lon, max_lat)`, or `None` when an
    /// axis-aligned bound isn't meaningful (3D: the whole world is
    /// "visible," and a planet-scale bbox isn't useful for hit-test
    /// culling). 2D always returns `Some`.
    fn visible_bounds(&self) -> Option<(f64, f64, f64, f64)>;
}

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

    /// Fingerprint of the projection's visible output.
    ///
    /// Stable across frames as long as `geo_to_screen` would produce
    /// identical results. Used by feature-level caches to avoid
    /// re-projecting thousands of coordinates every frame when the view
    /// is idle.
    pub fn fingerprint(&self) -> ProjectionFingerprint {
        ProjectionFingerprint {
            center_lat: self.center_lat.to_bits(),
            center_lon: self.center_lon.to_bits(),
            range_deg: self.range_deg.to_bits(),
            zoom: self.zoom.to_bits(),
            pan_x: self.pan_offset.x.to_bits(),
            pan_y: self.pan_offset.y.to_bits(),
            rect_min_x: self.screen_rect.min.x.to_bits(),
            rect_min_y: self.screen_rect.min.y.to_bits(),
            rect_max_x: self.screen_rect.max.x.to_bits(),
            rect_max_y: self.screen_rect.max.y.to_bits(),
        }
    }
}

impl Projection for MapProjection {
    fn geo_to_screen(&self, lat: f64, lon: f64) -> Option<Pos2> {
        Some(MapProjection::geo_to_screen(self, Coord { x: lon, y: lat }))
    }

    fn screen_to_geo(&self, pos: Pos2) -> Option<(f64, f64)> {
        let coord = MapProjection::screen_to_geo(self, pos);
        Some((coord.y, coord.x))
    }

    fn visible_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        Some(MapProjection::visible_bounds(self))
    }
}

/// Opaque signature of a [`MapProjection`]'s current output.
///
/// Two projections that produce the same fingerprint will map every
/// input coord to the same screen position. Constructed via
/// [`MapProjection::fingerprint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionFingerprint {
    center_lat: u64,
    center_lon: u64,
    range_deg: u64,
    zoom: u32,
    pan_x: u32,
    pan_y: u32,
    rect_min_x: u32,
    rect_min_y: u32,
    rect_max_x: u32,
    rect_max_y: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn proj(center_lat: f64, center_lon: f64) -> MapProjection {
        let mut p = MapProjection::new(center_lat, center_lon);
        p.update(
            1.0,
            Vec2::ZERO,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        );
        p
    }

    #[wasm_bindgen_test]
    fn center_coord_maps_to_screen_center() {
        let p = proj(39.0, -98.0);
        let screen = p.geo_to_screen(Coord { x: -98.0, y: 39.0 });
        let center = p.screen_rect.center();
        assert!((screen.x - center.x).abs() < 1e-3);
        assert!((screen.y - center.y).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn geo_screen_geo_round_trip_at_center() {
        let p = proj(39.0, -98.0);
        let original = Coord { x: -98.0, y: 39.0 };
        let screen = p.geo_to_screen(original);
        let back = p.screen_to_geo(screen);
        assert!((back.x - original.x).abs() < 1e-9);
        assert!((back.y - original.y).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn geo_screen_geo_round_trip_off_center() {
        let p = proj(39.0, -98.0);
        let original = Coord { x: -97.0, y: 39.5 };
        let screen = p.geo_to_screen(original);
        let back = p.screen_to_geo(screen);
        assert!((back.x - original.x).abs() < 1e-6);
        assert!((back.y - original.y).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn screen_to_geo_round_trip_at_arbitrary_pixel() {
        let p = proj(39.0, -98.0);
        let original = Pos2::new(500.0, 200.0);
        let geo = p.screen_to_geo(original);
        let back = p.geo_to_screen(geo);
        assert!((back.x - original.x).abs() < 1e-3);
        assert!((back.y - original.y).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn higher_zoom_shrinks_visible_bounds() {
        let mut p = proj(39.0, -98.0);
        let (min_lon1, min_lat1, max_lon1, max_lat1) = p.visible_bounds();
        p.update(4.0, Vec2::ZERO, p.screen_rect);
        let (min_lon4, min_lat4, max_lon4, max_lat4) = p.visible_bounds();
        let span1_lon = max_lon1 - min_lon1;
        let span4_lon = max_lon4 - min_lon4;
        let span1_lat = max_lat1 - min_lat1;
        let span4_lat = max_lat4 - min_lat4;
        assert!(span4_lon < span1_lon);
        assert!(span4_lat < span1_lat);
        // Doubling zoom halves the visible span; 4x zoom should be ~4x smaller.
        assert!((span1_lon / span4_lon - 4.0).abs() < 0.01);
    }

    #[wasm_bindgen_test]
    fn visible_bounds_min_le_max() {
        let p = proj(39.0, -98.0);
        let (min_lon, min_lat, max_lon, max_lat) = p.visible_bounds();
        assert!(min_lon <= max_lon);
        assert!(min_lat <= max_lat);
    }

    #[wasm_bindgen_test]
    fn is_visible_includes_center_excludes_far_point() {
        let p = proj(39.0, -98.0);
        assert!(p.is_visible(Coord { x: -98.0, y: 39.0 }, 0.0));
        assert!(!p.is_visible(Coord { x: 0.0, y: 0.0 }, 0.0));
    }

    #[wasm_bindgen_test]
    fn fingerprint_stable_when_inputs_unchanged() {
        let p = proj(39.0, -98.0);
        assert_eq!(p.fingerprint(), p.fingerprint());
    }

    #[wasm_bindgen_test]
    fn fingerprint_changes_when_zoom_changes() {
        let mut p = proj(39.0, -98.0);
        let f0 = p.fingerprint();
        p.update(2.0, p.pan_offset, p.screen_rect);
        assert_ne!(f0, p.fingerprint());
    }

    #[wasm_bindgen_test]
    fn fingerprint_changes_when_center_changes() {
        let f0 = proj(39.0, -98.0).fingerprint();
        let f1 = proj(40.0, -98.0).fingerprint();
        assert_ne!(f0, f1);
    }

    #[wasm_bindgen_test]
    fn pan_offset_translates_screen_position() {
        let p = proj(39.0, -98.0);
        let s0 = p.geo_to_screen(Coord { x: -98.0, y: 39.0 });
        let mut p2 = proj(39.0, -98.0);
        p2.update(1.0, Vec2::new(50.0, 30.0), p2.screen_rect);
        let s1 = p2.geo_to_screen(Coord { x: -98.0, y: 39.0 });
        assert!((s1.x - s0.x - 50.0).abs() < 1e-3);
        assert!((s1.y - s0.y - 30.0).abs() < 1e-3);
    }
}
