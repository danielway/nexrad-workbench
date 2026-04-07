//! NWS alert data model and GeoJSON parser.

use eframe::egui::Color32;
use geo_types::Coord;

/// Severity classification for NWS alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Extreme,
    Severe,
    Moderate,
    Minor,
    Unknown,
}

impl AlertSeverity {
    fn parse(s: &str) -> Self {
        match s {
            "Extreme" => Self::Extreme,
            "Severe" => Self::Severe,
            "Moderate" => Self::Moderate,
            "Minor" => Self::Minor,
            _ => Self::Unknown,
        }
    }

    /// Base color for this severity level.
    pub fn color(self) -> Color32 {
        match self {
            Self::Extreme => Color32::from_rgb(200, 0, 200),
            Self::Severe => Color32::from_rgb(255, 0, 0),
            Self::Moderate => Color32::from_rgb(255, 165, 0),
            Self::Minor => Color32::from_rgb(255, 255, 0),
            Self::Unknown => Color32::from_rgb(128, 128, 128),
        }
    }
}

/// A single parsed NWS weather alert.
#[derive(Debug, Clone)]
pub struct NwsAlert {
    /// Unique alert ID from the API (used for deduplication).
    #[allow(dead_code)]
    pub id: String,
    pub event: String,
    pub headline: Option<String>,
    pub description: String,
    pub instruction: Option<String>,
    pub severity: AlertSeverity,
    pub urgency: String,
    pub effective: String,
    pub expires: String,
    pub onset: Option<String>,
    /// Polygon vertices as (lon, lat) pairs from GeoJSON geometry.
    pub polygon: Option<Vec<Coord<f64>>>,
    /// Precomputed bounding box (min_lon, min_lat, max_lon, max_lat) for culling.
    pub bbox: Option<(f64, f64, f64, f64)>,
}

/// Map well-known NWS event names to standard NWS warning colors.
/// Falls back to severity-based color for unrecognized events.
pub fn event_color(event: &str, severity: AlertSeverity) -> Color32 {
    match event {
        "Tornado Warning" => Color32::from_rgb(255, 0, 0),
        "Tornado Watch" => Color32::from_rgb(255, 255, 0),
        "Severe Thunderstorm Warning" => Color32::from_rgb(255, 165, 0),
        "Severe Thunderstorm Watch" => Color32::from_rgb(219, 112, 147),
        "Flash Flood Warning" => Color32::from_rgb(139, 0, 0),
        "Flash Flood Watch" => Color32::from_rgb(46, 139, 87),
        "Flood Warning" => Color32::from_rgb(0, 255, 0),
        "Flood Watch" => Color32::from_rgb(46, 139, 87),
        "Winter Storm Warning" => Color32::from_rgb(255, 105, 180),
        "Winter Storm Watch" => Color32::from_rgb(70, 130, 180),
        "Blizzard Warning" => Color32::from_rgb(255, 69, 0),
        "Ice Storm Warning" => Color32::from_rgb(139, 0, 139),
        "Wind Advisory" => Color32::from_rgb(210, 180, 140),
        "Heat Advisory" => Color32::from_rgb(255, 127, 80),
        "Excessive Heat Warning" => Color32::from_rgb(199, 21, 133),
        "Dense Fog Advisory" => Color32::from_rgb(112, 128, 144),
        "Special Weather Statement" => Color32::from_rgb(255, 228, 181),
        "Dust Storm Warning" => Color32::from_rgb(255, 228, 196),
        _ => severity.color(),
    }
}

/// Short abbreviation for display in compact badges.
pub fn event_abbreviation(event: &str) -> &str {
    match event {
        "Tornado Warning" => "TOR",
        "Tornado Watch" => "TOA",
        "Severe Thunderstorm Warning" => "SVR",
        "Severe Thunderstorm Watch" => "SVA",
        "Flash Flood Warning" => "FFW",
        "Flash Flood Watch" => "FFA",
        "Flood Warning" => "FLW",
        "Flood Watch" => "FLA",
        "Winter Storm Warning" => "WSW",
        "Winter Storm Watch" => "WSA",
        "Blizzard Warning" => "BZW",
        "Ice Storm Warning" => "ISW",
        "Wind Advisory" => "WND",
        "Heat Advisory" => "HEA",
        "Excessive Heat Warning" => "EHW",
        "Dense Fog Advisory" => "FOG",
        "Special Weather Statement" => "SPS",
        _ => "WX",
    }
}

/// Parse a NWS alerts API GeoJSON response into a list of alerts.
pub fn parse_alerts_geojson(json: &str) -> Result<Vec<NwsAlert>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {}", e))?;

    let features = root
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'features' array".to_string())?;

    let mut alerts = Vec::with_capacity(features.len());

    for feature in features {
        let props = match feature.get("properties") {
            Some(p) => p,
            None => continue,
        };

        let id = feature
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let event = str_field(props, "event");
        let severity = AlertSeverity::parse(
            props
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown"),
        );

        // Parse polygon from geometry
        let (polygon, bbox) = parse_geometry(feature.get("geometry"));

        alerts.push(NwsAlert {
            id,
            event,
            headline: opt_str_field(props, "headline"),
            description: str_field(props, "description"),
            instruction: opt_str_field(props, "instruction"),
            severity,
            urgency: str_field(props, "urgency"),
            effective: str_field(props, "effective"),
            expires: str_field(props, "expires"),
            onset: opt_str_field(props, "onset"),
            polygon,
            bbox,
        });
    }

    Ok(alerts)
}

fn str_field(obj: &serde_json::Value, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str_field(obj: &serde_json::Value, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Bounding box (min_lon, min_lat, max_lon, max_lat).
type Bbox = (f64, f64, f64, f64);

/// Parse GeoJSON geometry into polygon vertices and bounding box.
fn parse_geometry(geometry: Option<&serde_json::Value>) -> (Option<Vec<Coord<f64>>>, Option<Bbox>) {
    let geom = match geometry {
        Some(g) if !g.is_null() => g,
        _ => return (None, None),
    };

    let geom_type = geom.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let coords = geom.get("coordinates");

    let rings = match geom_type {
        "Polygon" => coords.and_then(|c| c.as_array()),
        "MultiPolygon" => {
            // Use first polygon of multi-polygon
            coords
                .and_then(|c| c.as_array())
                .and_then(|polys| polys.first())
                .and_then(|p| p.as_array())
        }
        _ => return (None, None),
    };

    let ring = match rings.and_then(|r| r.first()).and_then(|r| r.as_array()) {
        Some(r) => r,
        None => return (None, None),
    };

    let mut vertices = Vec::with_capacity(ring.len());
    let mut min_lon = f64::MAX;
    let mut min_lat = f64::MAX;
    let mut max_lon = f64::MIN;
    let mut max_lat = f64::MIN;

    for point in ring {
        let pair = match point.as_array() {
            Some(p) if p.len() >= 2 => p,
            _ => continue,
        };
        let lon = match pair[0].as_f64() {
            Some(v) => v,
            None => continue,
        };
        let lat = match pair[1].as_f64() {
            Some(v) => v,
            None => continue,
        };

        min_lon = min_lon.min(lon);
        min_lat = min_lat.min(lat);
        max_lon = max_lon.max(lon);
        max_lat = max_lat.max(lat);

        vertices.push(Coord { x: lon, y: lat });
    }

    if vertices.is_empty() {
        return (None, None);
    }

    (Some(vertices), Some((min_lon, min_lat, max_lon, max_lat)))
}
