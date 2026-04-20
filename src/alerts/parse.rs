//! GeoJSON → `Alert` parsing.
//!
//! The NWS alerts endpoint returns a GeoJSON FeatureCollection. We parse only
//! the fields we display or use for filtering and skip features without
//! renderable geometry (zone-only alerts).

use super::types::{Alert, AlertGeometry, AlertSeverity, Ring};
use serde_json::Value;

/// Parsed response payload.
pub struct ParsedAlerts {
    pub alerts: Vec<Alert>,
}

/// Parse a complete alerts response body.
pub fn parse_response(body: &str) -> Result<ParsedAlerts, String> {
    let root: Value = serde_json::from_str(body).map_err(|e| format!("parse error: {}", e))?;

    let features = root
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "response missing 'features' array".to_string())?;

    let mut alerts = Vec::with_capacity(features.len());
    for feature in features {
        if let Some(alert) = parse_feature(feature) {
            alerts.push(alert);
        }
    }

    Ok(ParsedAlerts { alerts })
}

fn parse_feature(feature: &Value) -> Option<Alert> {
    let props = feature.get("properties")?;

    // Prefer the feature's top-level id; fall back to properties.id.
    let id = feature
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| props.get("id").and_then(|v| v.as_str()))?
        .to_string();

    let event = props
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("Alert")
        .to_string();

    let severity = props
        .get("severity")
        .and_then(|v| v.as_str())
        .map(AlertSeverity::parse)
        .unwrap_or(AlertSeverity::Unknown);

    let headline = string_field(props, "headline");
    let description = string_field(props, "description");
    let instruction = string_field(props, "instruction");
    let urgency = string_field(props, "urgency");
    let certainty = string_field(props, "certainty");
    let area_desc = string_field(props, "areaDesc");
    let sender = string_field(props, "senderName");

    let effective_secs = parse_iso_secs(props, "effective");
    let onset_secs = parse_iso_secs(props, "onset");
    let expires_secs = parse_iso_secs(props, "expires");
    let ends_secs = parse_iso_secs(props, "ends");

    let geometry = parse_geometry(feature.get("geometry"));
    if geometry.is_empty() {
        // Zone-only alerts — skip in v1.
        return None;
    }

    Some(Alert {
        id,
        event,
        headline,
        description,
        instruction,
        severity,
        urgency,
        certainty,
        area_desc,
        sender,
        effective_secs,
        onset_secs,
        expires_secs,
        ends_secs,
        geometry,
    })
}

fn string_field(props: &Value, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_iso_secs(props: &Value, key: &str) -> Option<f64> {
    let s = props.get(key)?.as_str()?;
    // NWS timestamps are ISO 8601 with timezone offset, e.g.
    // "2024-07-15T21:45:00-05:00". `chrono` parses these.
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp() as f64)
}

fn parse_geometry(geom: Option<&Value>) -> AlertGeometry {
    let mut out = AlertGeometry::default();
    let geom = match geom {
        Some(g) if !g.is_null() => g,
        _ => return out,
    };

    let ty = geom.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let coords = match geom.get("coordinates") {
        Some(c) => c,
        None => return out,
    };

    match ty {
        "Polygon" => {
            // coordinates: [ring, ring, ...]
            if let Some(polygon) = parse_polygon(coords) {
                out.polygons.push(polygon);
            }
        }
        "MultiPolygon" => {
            // coordinates: [polygon, polygon, ...]
            if let Some(arr) = coords.as_array() {
                for poly in arr {
                    if let Some(polygon) = parse_polygon(poly) {
                        out.polygons.push(polygon);
                    }
                }
            }
        }
        _ => {}
    }

    out.recompute_bbox();
    out
}

/// Parse a GeoJSON polygon (array of rings).
fn parse_polygon(value: &Value) -> Option<Vec<Ring>> {
    let rings = value.as_array()?;
    let mut out = Vec::with_capacity(rings.len());
    for ring_value in rings {
        let ring = parse_ring(ring_value)?;
        if ring.len() >= 3 {
            out.push(ring);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_ring(value: &Value) -> Option<Ring> {
    let pts = value.as_array()?;
    let mut ring = Vec::with_capacity(pts.len());
    for pt in pts {
        let pair = pt.as_array()?;
        let lon = pair.first()?.as_f64()?;
        let lat = pair.get(1)?.as_f64()?;
        ring.push((lon, lat));
    }
    Some(ring)
}
