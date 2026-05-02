//! mPING API response parsing.
//!
//! The API uses Django REST Framework pagination — top-level `count`,
//! `next`, `previous`, `results`. Each result has `id`, `obtime`,
//! `category`, `description`, and `geom: { type: "Point", coordinates: [lon, lat] }`.

use super::types::{ReportCategory, StormReport};
use serde_json::Value;

/// Outcome of a successful parse.
pub struct ParsedReports {
    pub reports: Vec<StormReport>,
    /// Total count reported by the server (may exceed `reports.len()` if we
    /// only fetched the first page).
    pub total_count: usize,
}

/// Parse a single page of the mPING reports response.
pub fn parse_response(body: &str) -> Result<ParsedReports, String> {
    let root: Value = serde_json::from_str(body).map_err(|e| format!("parse error: {}", e))?;

    let results = root
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "response missing 'results' array".to_string())?;

    let total_count = root
        .get("count")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(results.len());

    let mut reports = Vec::with_capacity(results.len());
    for item in results {
        if let Some(report) = parse_report(item) {
            reports.push(report);
        }
    }

    Ok(ParsedReports {
        reports,
        total_count,
    })
}

fn parse_report(item: &Value) -> Option<StormReport> {
    let id = item.get("id").and_then(|v| v.as_i64())?;

    let obtime_ms = item
        .get("obtime")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis() as f64)?;

    let category = item
        .get("category")
        .and_then(|v| v.as_str())
        .map(ReportCategory::parse)
        .unwrap_or(ReportCategory::Other);

    let description = item
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let coords = item.get("geom")?.get("coordinates")?.as_array()?;
    let lon = coords.first()?.as_f64()?;
    let lat = coords.get(1)?.as_f64()?;

    Some(StormReport {
        id,
        obtime_ms,
        category,
        description,
        lat,
        lon,
    })
}
