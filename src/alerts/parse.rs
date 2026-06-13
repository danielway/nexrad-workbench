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
    let affected_zones = parse_affected_zones(props);
    if geometry.is_empty() && affected_zones.is_empty() {
        // Nothing renderable and no zones to resolve later — drop it.
        return None;
    }

    // Inline (storm-based) geometry can be triangulated for the fill now;
    // zone-only alerts get triangulated after their zones resolve.
    let fill_triangles = super::types::triangulate_polygons(&geometry.polygons);

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
        affected_zones,
        fill_triangles,
    })
}

/// Extract `properties.affectedZones` — an array of zone API URLs whose
/// geometry can be resolved separately for alerts issued without a polygon.
fn parse_affected_zones(props: &Value) -> Vec<String> {
    props
        .get("affectedZones")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A FeatureCollection wrapper around one feature body string.
    fn fc(feature: &str) -> String {
        format!(r#"{{"type":"FeatureCollection","features":[{feature}]}}"#)
    }

    /// A square Polygon feature is parsed into one alert with vertices in
    /// `(lon, lat)` order and a bbox spanning the square. The lon/lat ordering
    /// assertion is the critical guard: GeoJSON coordinates are `[lon, lat]`,
    /// and a swap here would mirror every alert across the diagonal.
    #[wasm_bindgen_test]
    fn polygon_feature_parses_lon_lat_and_bbox() {
        // Square from lon -100..-99, lat 40..41. First vertex is the SW corner.
        let feature = r#"{
            "id": "urn:oid:1",
            "geometry": {
                "type": "Polygon",
                "coordinates": [[[-100.0, 40.0], [-99.0, 40.0], [-99.0, 41.0], [-100.0, 41.0], [-100.0, 40.0]]]
            },
            "properties": { "event": "Tornado Warning", "severity": "Extreme" }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert_eq!(parsed.alerts.len(), 1);
        let a = &parsed.alerts[0];
        assert_eq!(a.id, "urn:oid:1");
        assert_eq!(a.event, "Tornado Warning");
        assert_eq!(a.severity, AlertSeverity::Extreme);
        assert_eq!(a.geometry.polygons.len(), 1);
        let outer = &a.geometry.polygons[0][0];
        // First vertex: lon=-100 (x), lat=40 (y) — NOT (40, -100).
        assert_eq!(outer[0], (-100.0, 40.0));
        assert_eq!(outer[1], (-99.0, 40.0));
        // bbox = (min_lon, min_lat, max_lon, max_lat).
        assert_eq!(a.geometry.bbox, Some((-100.0, 40.0, -99.0, 41.0)));
        // Triangulated fill is non-empty for a valid square.
        assert!(!a.fill_triangles.is_empty());
    }

    /// A MultiPolygon yields one polygon entry per member ring.
    #[wasm_bindgen_test]
    fn multipolygon_parses_multiple_polygons() {
        let feature = r#"{
            "id": "mp1",
            "geometry": {
                "type": "MultiPolygon",
                "coordinates": [
                    [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]],
                    [[[5.0, 5.0], [6.0, 5.0], [6.0, 6.0], [5.0, 5.0]]]
                ]
            },
            "properties": { "event": "Flood Warning" }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        let a = &parsed.alerts[0];
        assert_eq!(a.geometry.polygons.len(), 2);
        // bbox spans both polygons.
        assert_eq!(a.geometry.bbox, Some((0.0, 0.0, 6.0, 6.0)));
    }

    /// A zone-only feature (null geometry + affectedZones) is kept so its
    /// footprint can be resolved later; geometry stays empty.
    #[wasm_bindgen_test]
    fn zone_only_feature_kept() {
        let feature = r#"{
            "id": "z1",
            "geometry": null,
            "properties": {
                "event": "Winter Weather Advisory",
                "affectedZones": ["https://api.weather.gov/zones/forecast/IAZ001"]
            }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert_eq!(parsed.alerts.len(), 1);
        let a = &parsed.alerts[0];
        assert!(a.geometry.is_empty());
        assert_eq!(a.affected_zones.len(), 1);
    }

    /// A feature with neither geometry nor affectedZones is dropped.
    #[wasm_bindgen_test]
    fn feature_with_no_geometry_and_no_zones_dropped() {
        let feature = r#"{
            "id": "drop",
            "geometry": null,
            "properties": { "event": "Special Weather Statement" }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert!(parsed.alerts.is_empty());
    }

    /// `id` falls back to `properties.id` when the feature has no top-level id.
    #[wasm_bindgen_test]
    fn id_falls_back_to_properties_id() {
        let feature = r#"{
            "geometry": null,
            "properties": {
                "id": "prop-id",
                "event": "Flood Watch",
                "affectedZones": ["z"]
            }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert_eq!(parsed.alerts[0].id, "prop-id");
    }

    /// A feature with no id anywhere is dropped (no selection key).
    #[wasm_bindgen_test]
    fn feature_without_any_id_dropped() {
        let feature = r#"{
            "geometry": null,
            "properties": { "event": "Flood Watch", "affectedZones": ["z"] }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert!(parsed.alerts.is_empty());
    }

    /// ISO-8601-with-offset timestamps parse to Unix seconds; malformed or
    /// absent timestamps yield `None`.
    #[wasm_bindgen_test]
    fn timestamps_parse_and_fall_back_to_none() {
        let feature = r#"{
            "id": "t1",
            "geometry": null,
            "properties": {
                "event": "Flood Watch",
                "affectedZones": ["z"],
                "effective": "1970-01-01T00:00:10+00:00",
                "expires": "not a date"
            }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        let a = &parsed.alerts[0];
        // 10 seconds past the epoch.
        assert_eq!(a.effective_secs, Some(10.0));
        // Garbage string → None.
        assert_eq!(a.expires_secs, None);
        // Absent field → None.
        assert_eq!(a.ends_secs, None);
    }

    /// A polygon whose every ring has fewer than 3 points is dropped, leaving
    /// the alert with empty geometry (kept only if zones exist).
    #[wasm_bindgen_test]
    fn degenerate_short_ring_polygon_dropped() {
        let feature = r#"{
            "id": "short",
            "geometry": {
                "type": "Polygon",
                "coordinates": [[[0.0, 0.0], [1.0, 1.0]]]
            },
            "properties": { "event": "Flood Warning", "affectedZones": ["z"] }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        let a = &parsed.alerts[0];
        // The 2-point ring made the polygon None → geometry empty.
        assert!(a.geometry.is_empty());
    }

    /// Missing severity defaults to `Unknown`.
    #[wasm_bindgen_test]
    fn missing_severity_defaults_unknown() {
        let feature = r#"{
            "id": "s1",
            "geometry": null,
            "properties": { "event": "Flood Warning", "affectedZones": ["z"] }
        }"#;
        let parsed = parse_response(&fc(feature)).unwrap();
        assert_eq!(parsed.alerts[0].severity, AlertSeverity::Unknown);
    }

    /// A body without a `features` array is an error.
    #[wasm_bindgen_test]
    fn missing_features_array_errors() {
        assert!(parse_response(r#"{"type":"FeatureCollection"}"#).is_err());
    }
}
