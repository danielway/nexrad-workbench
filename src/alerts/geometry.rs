//! Lightweight geometric tests used for filtering and hit-testing alerts.

use super::types::Alert;

/// True if the alert's bounding box intersects the given view bounds.
/// Bounds are `(min_lon, min_lat, max_lon, max_lat)`.
pub fn bbox_intersects(alert: &Alert, bounds: (f64, f64, f64, f64)) -> bool {
    let Some((amin_lon, amin_lat, amax_lon, amax_lat)) = alert.geometry.bbox else {
        return false;
    };
    let (min_lon, min_lat, max_lon, max_lat) = bounds;
    !(amax_lon < min_lon || amin_lon > max_lon || amax_lat < min_lat || amin_lat > max_lat)
}

/// True if `(lon, lat)` lies inside any polygon of `alert`, respecting holes.
///
/// Uses the even-odd ray-casting rule. Each outer ring is tested for
/// containment; if inside, hole rings invalidate the hit.
pub fn contains_point(alert: &Alert, lon: f64, lat: f64) -> bool {
    for polygon in &alert.geometry.polygons {
        let mut iter = polygon.iter();
        let Some(outer) = iter.next() else { continue };
        if point_in_ring(outer, lon, lat) {
            let in_hole = iter.any(|hole| point_in_ring(hole, lon, lat));
            if !in_hole {
                return true;
            }
        }
    }
    false
}

fn point_in_ring(ring: &[(f64, f64)], lon: f64, lat: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        let crosses = (yi > lat) != (yj > lat);
        if crosses {
            let dy = yj - yi;
            // crosses==true implies yi != yj, so dy is nonzero.
            let x_at_lat = (xj - xi) * (lat - yi) / dy + xi;
            if lon < x_at_lat {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::types::{Alert, AlertGeometry, AlertSeverity, Ring};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Build an alert from raw polygons (`[outer, hole, ...]` each), recomputing
    /// the bbox so `bbox_intersects` has something to test against.
    fn alert_with_polygons(polygons: Vec<Vec<Ring>>) -> Alert {
        let mut geometry = AlertGeometry {
            polygons,
            bbox: None,
        };
        geometry.recompute_bbox();
        Alert {
            id: "test".into(),
            event: "Test".into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            severity: AlertSeverity::Unknown,
            urgency: String::new(),
            certainty: String::new(),
            area_desc: String::new(),
            sender: String::new(),
            effective_secs: None,
            onset_secs: None,
            expires_secs: None,
            ends_secs: None,
            geometry,
            affected_zones: Vec::new(),
            fill_triangles: Vec::new(),
        }
    }

    /// A unit square ring from (0,0) to (10,10).
    fn square(min: f64, max: f64) -> Ring {
        vec![(min, min), (max, min), (max, max), (min, max), (min, min)]
    }

    #[wasm_bindgen_test]
    fn point_inside_square_is_contained() {
        let a = alert_with_polygons(vec![vec![square(0.0, 10.0)]]);
        assert!(contains_point(&a, 5.0, 5.0));
    }

    #[wasm_bindgen_test]
    fn point_outside_square_is_not_contained() {
        let a = alert_with_polygons(vec![vec![square(0.0, 10.0)]]);
        assert!(!contains_point(&a, 20.0, 5.0));
        assert!(!contains_point(&a, -1.0, 5.0));
    }

    #[wasm_bindgen_test]
    fn point_in_hole_is_not_contained() {
        // Outer square 0..10, hole 4..6 centered.
        let outer = square(0.0, 10.0);
        let hole = square(4.0, 6.0);
        let a = alert_with_polygons(vec![vec![outer, hole]]);
        // Inside the outer but inside the hole → NOT contained.
        assert!(!contains_point(&a, 5.0, 5.0));
        // Inside the outer but outside the hole → contained.
        assert!(contains_point(&a, 1.0, 1.0));
    }

    #[wasm_bindgen_test]
    fn multipolygon_hits_second_polygon() {
        let a = alert_with_polygons(vec![vec![square(0.0, 1.0)], vec![square(100.0, 110.0)]]);
        // A point only inside the second polygon is still contained.
        assert!(contains_point(&a, 105.0, 105.0));
        // Between the two polygons → not contained.
        assert!(!contains_point(&a, 50.0, 50.0));
    }

    #[wasm_bindgen_test]
    fn degenerate_ring_is_never_contained() {
        // A 2-point "ring" is degenerate; nothing is inside it.
        let a = alert_with_polygons(vec![vec![vec![(0.0, 0.0), (10.0, 10.0)]]]);
        assert!(!contains_point(&a, 5.0, 5.0));
    }

    #[wasm_bindgen_test]
    fn bbox_intersects_overlap_touch_and_disjoint() {
        let a = alert_with_polygons(vec![vec![square(0.0, 10.0)]]);
        // Overlapping bounds.
        assert!(bbox_intersects(&a, (5.0, 5.0, 15.0, 15.0)));
        // Touching at the corner (edge case: inclusive).
        assert!(bbox_intersects(&a, (10.0, 10.0, 20.0, 20.0)));
        // Fully disjoint to the right.
        assert!(!bbox_intersects(&a, (20.0, 20.0, 30.0, 30.0)));
        // Fully disjoint below.
        assert!(!bbox_intersects(&a, (0.0, -20.0, 10.0, -10.0)));
    }

    #[wasm_bindgen_test]
    fn bbox_intersects_false_when_bbox_none() {
        // Zone-only alert: empty geometry → bbox None → never intersects.
        let a = alert_with_polygons(vec![]);
        assert_eq!(a.geometry.bbox, None);
        assert!(!bbox_intersects(&a, (-180.0, -90.0, 180.0, 90.0)));
    }
}
