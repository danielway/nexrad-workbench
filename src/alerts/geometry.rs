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
