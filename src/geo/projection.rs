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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Default 800x600 projection at the given center, zoom 1.0, no pan.
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
    fn default_has_continental_us_center_and_range() {
        let d = MapProjection::default();
        assert!((d.center_lat - 39.0).abs() < 1e-12);
        assert!((d.center_lon - (-98.0)).abs() < 1e-12);
        assert!((d.range_deg - 4.5).abs() < 1e-12);
        assert!((d.zoom - 1.0).abs() < 1e-7);
        assert!(d.pan_offset.x == 0.0 && d.pan_offset.y == 0.0);
    }

    #[wasm_bindgen_test]
    fn new_overrides_center_keeps_other_defaults() {
        let p = MapProjection::new(45.0, -120.0);
        assert!((p.center_lat - 45.0).abs() < 1e-12);
        assert!((p.center_lon - (-120.0)).abs() < 1e-12);
        // Range/zoom remain the Default values.
        assert!((p.range_deg - 4.5).abs() < 1e-12);
        assert!((p.zoom - 1.0).abs() < 1e-7);
    }

    #[wasm_bindgen_test]
    fn update_sets_view_state() {
        let mut p = MapProjection::new(39.0, -98.0);
        let r = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(640.0, 480.0));
        p.update(3.0, Vec2::new(7.0, -4.0), r);
        assert!((p.zoom - 3.0).abs() < 1e-7);
        assert!(p.pan_offset.x == 7.0 && p.pan_offset.y == -4.0);
        assert!(p.screen_rect == r);
    }

    #[wasm_bindgen_test]
    fn north_is_up_east_is_right() {
        let p = proj(39.0, -98.0);
        let center = p.geo_to_screen(Coord { x: -98.0, y: 39.0 });
        // 1 degree north -> screen Y decreases (up).
        let north = p.geo_to_screen(Coord { x: -98.0, y: 40.0 });
        assert!(north.y < center.y);
        // 1 degree east -> screen X increases (right).
        let east = p.geo_to_screen(Coord { x: -97.0, y: 39.0 });
        assert!(east.x > center.x);
    }

    #[wasm_bindgen_test]
    fn geo_to_screen_exact_offsets_for_one_degree() {
        // center=(400,300), half_size=min(800,600)/2=300, eff_range=4.5,
        // cos(39deg)=0.7771459614569709.
        let p = proj(39.0, -98.0);
        let cos39 = 0.7771459614569709_f64;
        let east = p.geo_to_screen(Coord { x: -97.0, y: 39.0 });
        let expected_x = 400.0 + (1.0 * cos39 / 4.5) * 300.0; // ~451.8097
        assert!((east.x as f64 - expected_x).abs() < 1e-2);
        assert!((east.y as f64 - 300.0).abs() < 1e-3);

        let north = p.geo_to_screen(Coord { x: -98.0, y: 40.0 });
        let expected_y = 300.0 + (-1.0 / 4.5) * 300.0; // ~233.3333
        assert!((north.y as f64 - expected_y).abs() < 1e-2);
        assert!((north.x as f64 - 400.0).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn half_size_uses_min_dimension() {
        // With an 800x600 rect, the projection scales by 300 (min/2), not 400.
        // Moving from center to the top edge (Y from 300 to 0) spans exactly
        // one half_size of normalized space, i.e. eff_range degrees of latitude.
        let p = proj(39.0, -98.0);
        let top_geo = p.screen_to_geo(Pos2::new(400.0, 0.0));
        // norm_y = -300/300 = -1 -> rel_lat = +4.5
        assert!((top_geo.y - (39.0 + 4.5)).abs() < 1e-9);
        assert!((top_geo.x - (-98.0)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn lat_correction_compresses_longitude_at_high_latitude() {
        // The same 1-degree longitude offset projects to fewer screen pixels
        // at a higher center latitude because cos(lat) shrinks.
        let low = proj(20.0, -98.0);
        let high = proj(60.0, -98.0);
        let c_low = low.geo_to_screen(Coord { x: -98.0, y: 20.0 });
        let e_low = low.geo_to_screen(Coord { x: -97.0, y: 20.0 });
        let c_high = high.geo_to_screen(Coord { x: -98.0, y: 60.0 });
        let e_high = high.geo_to_screen(Coord { x: -97.0, y: 60.0 });
        let dx_low = (e_low.x - c_low.x).abs();
        let dx_high = (e_high.x - c_high.x).abs();
        assert!(dx_high < dx_low);
    }

    #[wasm_bindgen_test]
    fn visible_bounds_lat_span_is_twice_range() {
        // Latitude span is unaffected by lat_correction: top/bottom edges are
        // exactly +/- eff_range from center, so the span is 2*range_deg.
        let p = proj(39.0, -98.0);
        let (min_lon, min_lat, max_lon, max_lat) = p.visible_bounds();
        assert!((max_lat - min_lat - 9.0).abs() < 1e-9);
        // Bounds are centered on the site latitude.
        assert!(((min_lat + max_lat) / 2.0 - 39.0).abs() < 1e-9);
        // Longitude bounds are centered on the site longitude.
        assert!(((min_lon + max_lon) / 2.0 - (-98.0)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn visible_bounds_lon_span_widened_by_lat_correction() {
        // half_size=300, center_x=400 -> norm_x spans [-4/3, 4/3].
        // lon span = (8/3) * eff_range / cos(39deg).
        let p = proj(39.0, -98.0);
        let (min_lon, _, max_lon, _) = p.visible_bounds();
        let cos39 = 0.7771459614569709_f64;
        let expected = (8.0 / 3.0) * 4.5 / cos39; // ~15.4411
        assert!((max_lon - min_lon - expected).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn is_visible_margin_extends_inclusion() {
        let p = proj(39.0, -98.0);
        // lon -90.0 is just outside max_lon (~-90.279) without margin...
        let pt = Coord { x: -90.0, y: 39.0 };
        assert!(!p.is_visible(pt, 0.0));
        // ...but a 1-degree margin pulls it inside.
        assert!(p.is_visible(pt, 1.0));
    }

    #[wasm_bindgen_test]
    fn bbox_visible_box_inside_is_visible() {
        let p = proj(39.0, -98.0);
        // Box well within visible bounds.
        assert!(p.bbox_visible(-99.0, 38.0, -97.0, 40.0));
    }

    #[wasm_bindgen_test]
    fn bbox_visible_far_box_not_visible() {
        let p = proj(39.0, -98.0);
        // Box far to the south-east near (0,0); outside visible bounds + margin.
        assert!(!p.bbox_visible(-1.0, -1.0, 1.0, 1.0));
    }

    #[wasm_bindgen_test]
    fn bbox_visible_each_disjoint_axis() {
        let p = proj(39.0, -98.0);
        // visible ~ lon[-105.72,-90.28] lat[34.5,43.5], margin=1.0.
        // Entirely west of view (max_lon < vis_min_lon - 1).
        assert!(!p.bbox_visible(-120.0, 38.0, -110.0, 40.0));
        // Entirely east of view (min_lon > vis_max_lon + 1).
        assert!(!p.bbox_visible(-80.0, 38.0, -70.0, 40.0));
        // Entirely south of view (max_lat < vis_min_lat - 1).
        assert!(!p.bbox_visible(-99.0, 20.0, -97.0, 30.0));
        // Entirely north of view (min_lat > vis_max_lat + 1).
        assert!(!p.bbox_visible(-99.0, 50.0, -97.0, 60.0));
    }

    #[wasm_bindgen_test]
    fn bbox_visible_overlapping_edge_within_margin_is_visible() {
        let p = proj(39.0, -98.0);
        // A box whose max_lon is just inside the west margin: vis_min_lon ~ -105.72,
        // margin 1.0 => threshold -106.72. max_lon = -106.0 is NOT < -106.72, so visible.
        assert!(p.bbox_visible(-110.0, 38.0, -106.0, 40.0));
    }

    #[wasm_bindgen_test]
    fn fingerprint_changes_with_pan_and_rect() {
        let mut p = proj(39.0, -98.0);
        let f0 = p.fingerprint();
        p.update(1.0, Vec2::new(5.0, 0.0), p.screen_rect);
        let f_pan = p.fingerprint();
        assert!(f0 != f_pan);

        let mut q = proj(39.0, -98.0);
        let g0 = q.fingerprint();
        q.update(
            1.0,
            Vec2::ZERO,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0)),
        );
        assert!(g0 != q.fingerprint());
    }

    #[wasm_bindgen_test]
    fn fingerprint_changes_when_lon_changes() {
        // Sibling tests cover lat; cover the longitude field of the fingerprint.
        let f0 = proj(39.0, -98.0).fingerprint();
        let f1 = proj(39.0, -97.0).fingerprint();
        assert!(f0 != f1);
    }

    #[wasm_bindgen_test]
    fn trait_impl_swaps_latlon_argument_order() {
        // The trait method takes (lat, lon) and must match the inherent
        // geo_to_screen(Coord{x:lon,y:lat}).
        let p = proj(39.0, -98.0);
        let inherent = MapProjection::geo_to_screen(&p, Coord { x: -97.0, y: 40.0 });
        let via_trait = Projection::geo_to_screen(&p, 40.0, -97.0);
        let s = via_trait.expect("2D projection always returns Some");
        assert!((s.x - inherent.x).abs() < 1e-4);
        assert!((s.y - inherent.y).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn trait_screen_to_geo_returns_lat_lon_tuple() {
        let p = proj(39.0, -98.0);
        let pos = Pos2::new(500.0, 200.0);
        let coord = MapProjection::screen_to_geo(&p, pos);
        let (lat, lon) = Projection::screen_to_geo(&p, pos).expect("2D always Some");
        // Trait returns (lat, lon) = (coord.y, coord.x).
        assert!((lat - coord.y).abs() < 1e-9);
        assert!((lon - coord.x).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn trait_visible_bounds_matches_inherent() {
        let p = proj(39.0, -98.0);
        let (a, b, c, d) = MapProjection::visible_bounds(&p);
        let (ta, tb, tc, td) = Projection::visible_bounds(&p).expect("2D always Some");
        assert!((a - ta).abs() < 1e-12);
        assert!((b - tb).abs() < 1e-12);
        assert!((c - tc).abs() < 1e-12);
        assert!((d - td).abs() < 1e-12);
    }
}
