//! NWS alert data types.
//!
//! These types are a simplified projection of the GeoJSON documents returned
//! by `https://api.weather.gov/alerts/active`. We only extract the fields we
//! display or use for filtering.

/// Severity classification per the Common Alerting Protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Extreme,
    Severe,
    Moderate,
    Minor,
    Unknown,
}

impl AlertSeverity {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "extreme" => Self::Extreme,
            "severe" => Self::Severe,
            "moderate" => Self::Moderate,
            "minor" => Self::Minor,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Extreme => "Extreme",
            Self::Severe => "Severe",
            Self::Moderate => "Moderate",
            Self::Minor => "Minor",
            Self::Unknown => "Unknown",
        }
    }

    /// Numeric rank so callers can sort highest-severity first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Extreme => 4,
            Self::Severe => 3,
            Self::Moderate => 2,
            Self::Minor => 1,
            Self::Unknown => 0,
        }
    }
}

/// RGB color for an alert chosen by **event type** (hazard family) rather than
/// CAP severity. Used in the top bar, modal, and canvas overlay.
///
/// Hue encodes the hazard — red for tornadoes, yellow for thunderstorms, green
/// for floods, tan for special weather statements, and so on. Brightness
/// encodes the product class: warnings render at full intensity, watches dimmer,
/// and advisories/statements dimmer still, so the most urgent products read
/// brightest. Unmapped events fall back to a neutral blue-gray.
pub fn event_color(event: &str) -> (u8, u8, u8) {
    let e = event.to_ascii_lowercase();

    // Base hue by hazard family.
    let base = if e.contains("tornado") {
        (255, 50, 50) // red
    } else if e.contains("thunderstorm") {
        (240, 220, 40) // yellow
    } else if e.contains("flood") {
        (60, 200, 90) // green
    } else if e.contains("special weather statement") {
        (210, 180, 120) // tan
    } else if e.contains("snow")
        || e.contains("winter")
        || e.contains("blizzard")
        || e.contains("ice")
        || e.contains("freez")
        || e.contains("frost")
        || e.contains("sleet")
    {
        (120, 160, 240) // icy blue
    } else if e.contains("fire") || e.contains("red flag") || e.contains("smoke") {
        (255, 120, 40) // orange
    } else if e.contains("heat") {
        (240, 100, 140) // magenta
    } else if e.contains("wind") || e.contains("dust") {
        (200, 170, 110) // dusty gold
    } else if e.contains("marine")
        || e.contains("surf")
        || e.contains("rip current")
        || e.contains("coastal")
        || e.contains("tsunami")
        || e.contains("seiche")
    {
        (80, 200, 210) // teal
    } else if e.contains("fog") {
        (160, 160, 170) // gray
    } else {
        (150, 170, 200) // neutral default
    };

    // Dim watches/advisories/statements relative to warnings.
    let scale = if e.contains("warning") {
        1.0
    } else if e.contains("watch") {
        0.78
    } else {
        0.62 // advisory, statement, other
    };

    (
        (base.0 as f32 * scale) as u8,
        (base.1 as f32 * scale) as u8,
        (base.2 as f32 * scale) as u8,
    )
}

/// A polygon ring is a closed sequence of (lon, lat) vertices.
pub type Ring = Vec<(f64, f64)>;

/// An alert's spatial footprint. A MultiPolygon is a list of polygons; each
/// polygon is an outer ring followed by zero or more holes.
#[derive(Debug, Clone, Default)]
pub struct AlertGeometry {
    /// Polygons; each polygon is [outer_ring, hole_ring, hole_ring, ...].
    pub polygons: Vec<Vec<Ring>>,
    /// Precomputed bounding box (min_lon, min_lat, max_lon, max_lat).
    pub bbox: Option<(f64, f64, f64, f64)>,
}

impl AlertGeometry {
    /// True if this geometry is empty (e.g. zone-only alerts without
    /// resolved geometry).
    pub fn is_empty(&self) -> bool {
        self.polygons.is_empty()
    }

    /// Recompute bbox from `polygons`. Call after mutating `polygons`.
    pub fn recompute_bbox(&mut self) {
        let mut min_lon = f64::INFINITY;
        let mut min_lat = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut any = false;
        for polygon in &self.polygons {
            for ring in polygon {
                for &(lon, lat) in ring {
                    if lon < min_lon {
                        min_lon = lon;
                    }
                    if lon > max_lon {
                        max_lon = lon;
                    }
                    if lat < min_lat {
                        min_lat = lat;
                    }
                    if lat > max_lat {
                        max_lat = lat;
                    }
                    any = true;
                }
            }
        }
        self.bbox = if any {
            Some((min_lon, min_lat, max_lon, max_lat))
        } else {
            None
        };
    }
}

/// A single active NWS alert. Geometry may be empty for zone-only alerts;
/// those are filtered out before reaching the UI.
#[derive(Debug, Clone)]
pub struct Alert {
    /// Stable identifier (from GeoJSON feature `id`). Used as a selection key.
    pub id: String,
    /// Event name (e.g. "Tornado Warning", "Flood Advisory").
    pub event: String,
    /// One-line headline (properties.headline), may be empty.
    pub headline: String,
    /// Long-form description (properties.description), may be empty.
    pub description: String,
    /// Recommended action (properties.instruction), may be empty.
    pub instruction: String,
    /// Classification.
    pub severity: AlertSeverity,
    /// Urgency (Immediate, Expected, Future, Past, Unknown) — raw string.
    pub urgency: String,
    /// Certainty (Observed, Likely, Possible, Unlikely, Unknown) — raw string.
    pub certainty: String,
    /// Human-readable list of affected areas.
    pub area_desc: String,
    /// Issuing office / sender (e.g. "NWS Des Moines IA").
    pub sender: String,
    /// Effective timestamp (Unix seconds). None if unparseable.
    pub effective_secs: Option<f64>,
    /// Onset timestamp (Unix seconds). None if unparseable.
    pub onset_secs: Option<f64>,
    /// Expiration timestamp (Unix seconds). None if unparseable.
    pub expires_secs: Option<f64>,
    /// Ends timestamp (Unix seconds). None if unparseable.
    pub ends_secs: Option<f64>,
    /// Spatial footprint. May be empty for zone-only alerts until the zones
    /// in `affected_zones` are resolved to geometry.
    pub geometry: AlertGeometry,
    /// Zone API URLs (`properties.affectedZones`). Used to resolve a footprint
    /// for alerts the NWS issues without an inline polygon.
    pub affected_zones: Vec<String>,
    /// Pre-triangulated geometry (geo-space `(lon, lat)` triangles) for the
    /// translucent fill. Computed once when `geometry` is finalized so rendering
    /// only projects cached triangles. Empty until geometry is known.
    pub fill_triangles: Vec<[(f64, f64); 3]>,
}

/// Triangulate alert polygons (each `[outer, hole, …]`) into geo-space
/// triangles for fill rendering. Earcut tolerates the duplicate/near-collinear
/// vertices left by zone simplification.
pub(crate) fn triangulate_polygons(polygons: &[Vec<Ring>]) -> Vec<[(f64, f64); 3]> {
    use geo::TriangulateEarcut;
    let mut tris = Vec::new();
    for rings in polygons {
        let mut iter = rings.iter();
        let Some(exterior) = iter.next() else {
            continue;
        };
        let poly = geo::Polygon::new(
            geo::LineString::from(exterior.clone()),
            iter.map(|r| geo::LineString::from(r.clone())).collect(),
        );
        let raw = poly.earcut_triangles_raw();
        let v = &raw.vertices; // flat [x0, y0, x1, y1, …]
        for idx in raw.triangle_indices.chunks_exact(3) {
            let pt = |i: usize| (v[2 * i], v[2 * i + 1]);
            tris.push([pt(idx[0]), pt(idx[1]), pt(idx[2])]);
        }
    }
    tris
}

impl Alert {
    /// RGB color for this alert, chosen by event type. See [`event_color`].
    pub fn color(&self) -> (u8, u8, u8) {
        event_color(&self.event)
    }

    /// True if this alert is a warning (vs a watch/advisory/statement) — i.e.
    /// the event name contains "warning" (case-insensitive). This is the same
    /// distinction [`event_color`] uses to brighten warnings over watches, and
    /// it drives the count split, map de-emphasis, and layer toggles.
    pub fn is_warning(&self) -> bool {
        self.event.to_ascii_lowercase().contains("warning")
    }

    /// True when the alert has an end timestamp in the past.
    pub fn is_expired(&self, now_secs: f64) -> bool {
        let end = self.ends_secs.or(self.expires_secs);
        matches!(end, Some(t) if t < now_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn alert(event: &str, ends: Option<f64>, expires: Option<f64>) -> Alert {
        Alert {
            id: "t".into(),
            event: event.into(),
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
            expires_secs: expires,
            ends_secs: ends,
            geometry: AlertGeometry::default(),
            affected_zones: Vec::new(),
            fill_triangles: Vec::new(),
        }
    }

    /// `parse` is case-insensitive and defaults to `Unknown`; rank orders the
    /// severities highest-first.
    #[wasm_bindgen_test]
    fn severity_parse_and_rank() {
        assert_eq!(AlertSeverity::parse("Extreme"), AlertSeverity::Extreme);
        assert_eq!(AlertSeverity::parse("SEVERE"), AlertSeverity::Severe);
        assert_eq!(AlertSeverity::parse("  moderate "), AlertSeverity::Moderate);
        assert_eq!(AlertSeverity::parse("minor"), AlertSeverity::Minor);
        // Garbage / empty → Unknown.
        assert_eq!(AlertSeverity::parse("???"), AlertSeverity::Unknown);
        assert_eq!(AlertSeverity::parse(""), AlertSeverity::Unknown);
        // Rank order is strictly descending by severity.
        assert!(
            AlertSeverity::Extreme.rank() > AlertSeverity::Severe.rank()
                && AlertSeverity::Severe.rank() > AlertSeverity::Moderate.rank()
                && AlertSeverity::Moderate.rank() > AlertSeverity::Minor.rank()
                && AlertSeverity::Minor.rank() > AlertSeverity::Unknown.rank()
        );
    }

    /// `event_color` picks a hue family per hazard keyword.
    #[wasm_bindgen_test]
    fn event_color_hue_families() {
        // Tornado warning → red (R dominant).
        let (r, g, b) = event_color("Tornado Warning");
        assert!(r > g && r > b, "tornado not red: {r},{g},{b}");
        // Thunderstorm → yellow (R and G high, B low).
        let (r, g, b) = event_color("Severe Thunderstorm Warning");
        assert!(
            r > 100 && g > 100 && b < 100,
            "tstorm not yellow: {r},{g},{b}"
        );
        // Flood → green (G dominant).
        let (r, g, b) = event_color("Flood Warning");
        assert!(g > r && g > b, "flood not green: {r},{g},{b}");
        // Unmapped → neutral blue-gray (B highest of the three).
        let (r, g, b) = event_color("Earthquake Warning");
        assert!(b >= r && b >= g, "default not blue-gray: {r},{g},{b}");
    }

    /// For the same hazard family, warnings render brighter than watches,
    /// which render brighter than advisories/statements.
    #[wasm_bindgen_test]
    fn event_color_brightness_warning_gt_watch_gt_advisory() {
        let warn = event_color("Flood Warning");
        let watch = event_color("Flood Watch");
        let adv = event_color("Flood Advisory");
        // Compare on the dominant green channel.
        assert!(
            warn.1 > watch.1,
            "warning {} not > watch {}",
            warn.1,
            watch.1
        );
        assert!(
            watch.1 > adv.1,
            "watch {} not > advisory {}",
            watch.1,
            adv.1
        );
    }

    /// `is_warning` is case-insensitive and false for watches/advisories.
    #[wasm_bindgen_test]
    fn is_warning_case_insensitive() {
        assert!(alert("Tornado WARNING", None, None).is_warning());
        assert!(alert("flood warning", None, None).is_warning());
        assert!(!alert("Flood Watch", None, None).is_warning());
        assert!(!alert("Winter Weather Advisory", None, None).is_warning());
    }

    /// `is_expired` uses `ends_secs` first, falling back to `expires_secs`, and
    /// the comparison is strict (`< now`).
    #[wasm_bindgen_test]
    fn is_expired_prefers_ends_then_expires() {
        // ends in the past → expired (expires irrelevant).
        assert!(alert("e", Some(100.0), Some(9999.0)).is_expired(200.0));
        // ends in the future → not expired even though expires is past.
        assert!(!alert("e", Some(9999.0), Some(50.0)).is_expired(200.0));
        // no ends → fall back to expires (past).
        assert!(alert("e", None, Some(100.0)).is_expired(200.0));
        // no ends, expires in the future → not expired.
        assert!(!alert("e", None, Some(300.0)).is_expired(200.0));
        // neither → never expired.
        assert!(!alert("e", None, None).is_expired(200.0));
        // boundary: end exactly == now is NOT expired (strict <).
        assert!(!alert("e", Some(200.0), None).is_expired(200.0));
        // one tick before now → expired.
        assert!(alert("e", Some(199.9), None).is_expired(200.0));
    }

    /// `recompute_bbox` is `None` for empty geometry and a tight AABB
    /// otherwise.
    #[wasm_bindgen_test]
    fn recompute_bbox_empty_and_aabb() {
        let mut empty = AlertGeometry::default();
        empty.recompute_bbox();
        assert_eq!(empty.bbox, None);

        let mut g = AlertGeometry {
            polygons: vec![vec![vec![
                (-100.0, 40.0),
                (-90.0, 40.0),
                (-95.0, 45.0),
                (-100.0, 40.0),
            ]]],
            bbox: None,
        };
        g.recompute_bbox();
        assert_eq!(g.bbox, Some((-100.0, 40.0, -90.0, 45.0)));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn alert_ev(event: &str) -> Alert {
        Alert {
            id: "t".into(),
            event: event.into(),
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
            geometry: AlertGeometry::default(),
            affected_zones: Vec::new(),
            fill_triangles: Vec::new(),
        }
    }

    /// Triangle area via the shoelace formula (absolute).
    fn tri_area(t: &[(f64, f64); 3]) -> f64 {
        let [a, b, c] = t;
        0.5 * ((b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1)).abs()
    }

    /// `label` returns the exact display string for every variant (untested by
    /// the existing suite).
    #[wasm_bindgen_test]
    fn severity_label_all_variants() {
        assert_eq!(AlertSeverity::Extreme.label(), "Extreme");
        assert_eq!(AlertSeverity::Severe.label(), "Severe");
        assert_eq!(AlertSeverity::Moderate.label(), "Moderate");
        assert_eq!(AlertSeverity::Minor.label(), "Minor");
        assert_eq!(AlertSeverity::Unknown.label(), "Unknown");
    }

    /// `rank` returns the exact numeric values, not just a monotone ordering.
    #[wasm_bindgen_test]
    fn severity_rank_exact_values() {
        assert_eq!(AlertSeverity::Extreme.rank(), 4);
        assert_eq!(AlertSeverity::Severe.rank(), 3);
        assert_eq!(AlertSeverity::Moderate.rank(), 2);
        assert_eq!(AlertSeverity::Minor.rank(), 1);
        assert_eq!(AlertSeverity::Unknown.rank(), 0);
    }

    /// `parse` accepts the canonical lowercased keywords exactly; near-miss
    /// strings fall through to `Unknown`.
    #[wasm_bindgen_test]
    fn severity_parse_near_misses() {
        // Embedded but not exact (after trim) → Unknown.
        assert_eq!(AlertSeverity::parse("very severe"), AlertSeverity::Unknown);
        assert_eq!(AlertSeverity::parse("extremely"), AlertSeverity::Unknown);
        // Leading/trailing whitespace is trimmed; internal case ignored.
        assert_eq!(AlertSeverity::parse("\tSeVeRe\n"), AlertSeverity::Severe);
    }

    /// The hazard families the existing suite does not exercise, asserted at the
    /// warning scale (1.0) so the base RGB is returned verbatim.
    #[wasm_bindgen_test]
    fn event_color_remaining_families_at_warning_scale() {
        // Icy blue family — multiple keywords all map to the same base.
        assert!(event_color("Winter Storm Warning") == (120, 160, 240));
        assert!(event_color("Blizzard Warning") == (120, 160, 240));
        assert!(event_color("Freeze Warning") == (120, 160, 240));
        assert!(event_color("Ice Storm Warning") == (120, 160, 240));
        // Fire / smoke → orange.
        assert!(event_color("Fire Weather Warning") == (255, 120, 40));
        assert!(event_color("Red Flag Warning") == (255, 120, 40));
        // Heat → magenta.
        assert!(event_color("Excessive Heat Warning") == (240, 100, 140));
        // Wind / dust → dusty gold.
        assert!(event_color("High Wind Warning") == (200, 170, 110));
        assert!(event_color("Dust Storm Warning") == (200, 170, 110));
        // Marine family → teal.
        assert!(event_color("Tsunami Warning") == (80, 200, 210));
        assert!(event_color("Rip Current Warning") == (80, 200, 210));
        // Fog → gray.
        assert!(event_color("Dense Fog Warning") == (160, 160, 170));
    }

    /// Brightness scale is exact: warning=1.0, watch=0.78, advisory/other=0.62,
    /// applied as `(base * scale) as u8` (truncating).
    #[wasm_bindgen_test]
    fn event_color_exact_scale_arithmetic() {
        // Flood base (60, 200, 90).
        assert!(event_color("Flood Warning") == (60, 200, 90));
        assert!(event_color("Flood Watch") == (46, 156, 70));
        assert!(event_color("Flood Advisory") == (37, 124, 55));
        // Tornado base (255, 50, 50) at watch scale 0.78.
        assert!(event_color("Tornado Watch") == (198, 39, 39));
    }

    /// An event with neither "warning" nor "watch" uses the 0.62 "other" branch.
    /// "Special Weather Statement" also exercises the tan hue family.
    #[wasm_bindgen_test]
    fn event_color_statement_tan_other_scale() {
        // Tan base (210, 180, 120) * 0.62 (no warning/watch keyword).
        assert!(event_color("Special Weather Statement") == (130, 111, 74));
    }

    /// Hue keyword precedence: earlier `else if` arms win when several keywords
    /// are present. "Coastal Flood" hits the flood arm (green) before the marine
    /// arm (teal); "Tornado ... Thunderstorm" hits tornado (red) before yellow.
    #[wasm_bindgen_test]
    fn event_color_keyword_precedence() {
        // flood is checked before coastal/marine → green, not teal.
        assert!(event_color("Coastal Flood Warning") == (60, 200, 90));
        // tornado is checked before thunderstorm → red base.
        let (r, g, b) = event_color("Tornado and Thunderstorm Warning");
        assert!(r > g && r > b, "expected red base, got {r},{g},{b}");
        assert!((r, g, b) == (255, 50, 50));
    }

    /// `Alert::color` delegates to `event_color` on the event name.
    #[wasm_bindgen_test]
    fn alert_color_delegates() {
        assert!(alert_ev("Tornado Warning").color() == event_color("Tornado Warning"));
        assert!(alert_ev("Flood Watch").color() == (46, 156, 70));
    }

    /// `is_empty` reflects whether any polygon is present.
    #[wasm_bindgen_test]
    fn geometry_is_empty() {
        assert!(AlertGeometry::default().is_empty());
        let g = AlertGeometry {
            polygons: vec![vec![vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]]],
            bbox: None,
        };
        assert!(!g.is_empty());
    }

    /// `recompute_bbox` unions across multiple disjoint polygons and across
    /// hole rings — the AABB must enclose every vertex of every ring.
    #[wasm_bindgen_test]
    fn recompute_bbox_multi_polygon_and_holes() {
        let mut g = AlertGeometry {
            polygons: vec![
                // Polygon 0: outer ring near the origin + an inner hole ring.
                vec![
                    vec![
                        (0.0, 0.0),
                        (10.0, 0.0),
                        (10.0, 10.0),
                        (0.0, 10.0),
                        (0.0, 0.0),
                    ],
                    vec![(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 2.0)],
                ],
                // Polygon 1: a disjoint polygon extending the bounds far out.
                vec![vec![(-5.0, -3.0), (-1.0, -3.0), (-3.0, 25.0), (-5.0, -3.0)]],
            ],
            bbox: None,
        };
        g.recompute_bbox();
        // min_lon from polygon 1 (-5), max_lon from polygon 0 (10),
        // min_lat from polygon 1 (-3), max_lat from polygon 1 (25).
        assert_eq!(g.bbox, Some((-5.0, -3.0, 10.0, 25.0)));
    }

    /// `triangulate_polygons`: a simple square yields a fan whose total area
    /// equals the square's area; polygons with no exterior ring are skipped and
    /// an empty input yields no triangles.
    #[wasm_bindgen_test]
    fn triangulate_polygons_area_and_skips() {
        // 3x3 axis-aligned square (closed ring) → triangles summing to area 9.
        let square: Vec<Vec<Ring>> = vec![vec![vec![
            (0.0, 0.0),
            (3.0, 0.0),
            (3.0, 3.0),
            (0.0, 3.0),
            (0.0, 0.0),
        ]]];
        let tris = triangulate_polygons(&square);
        assert!(!tris.is_empty());
        let total: f64 = tris.iter().map(tri_area).sum();
        assert!((total - 9.0).abs() < 1e-9, "total area {total} != 9");

        // A polygon entry with an empty rings list is skipped (no exterior).
        let no_exterior: Vec<Vec<Ring>> = vec![Vec::new()];
        assert!(triangulate_polygons(&no_exterior).is_empty());

        // Empty input → empty output.
        let none: Vec<Vec<Ring>> = Vec::new();
        assert!(triangulate_polygons(&none).is_empty());
    }
}
