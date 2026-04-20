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

    /// RGB color for this severity (used in top bar, modal, and canvas overlay).
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            Self::Extreme => (220, 40, 40),
            Self::Severe => (240, 130, 30),
            Self::Moderate => (230, 200, 50),
            Self::Minor | Self::Unknown => (120, 180, 230),
        }
    }
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
    /// Spatial footprint.
    pub geometry: AlertGeometry,
}

impl Alert {
    /// True when the alert has an end timestamp in the past.
    pub fn is_expired(&self, now_secs: f64) -> bool {
        let end = self.ends_secs.or(self.expires_secs);
        matches!(end, Some(t) if t < now_secs)
    }
}
