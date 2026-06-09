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
