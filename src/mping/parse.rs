//! mPING API response parsing.
//!
//! The API uses Django REST Framework pagination — top-level `count`,
//! `next`, `previous`, `results`. Each result has `id`, `obtime`,
//! `category`, `description`, and `geom: { type: "Point", coordinates: [lon, lat] }`.

use super::types::{ReportCategory, StormReport};
use serde_json::Value;

/// Outcome of a successful parse.
pub(crate) struct ParsedReports {
    pub reports: Vec<StormReport>,
    /// Total count reported by the server (may exceed `reports.len()` if we
    /// only fetched the first page).
    pub total_count: usize,
}

/// Parse a single page of the mPING reports response.
pub(crate) fn parse_response(body: &str) -> Result<ParsedReports, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// One result object as a JSON string.
    fn result(id: i64, obtime: &str, category: &str, lon: f64, lat: f64) -> String {
        format!(
            r#"{{"id":{id},"obtime":"{obtime}","category":"{category}","description":"d","geom":{{"type":"Point","coordinates":[{lon},{lat}]}}}}"#
        )
    }

    /// Wrap result strings in a paginated body with an explicit `count`.
    fn body(count: Option<usize>, results: &[String]) -> String {
        let results = results.join(",");
        match count {
            Some(c) => format!(r#"{{"count":{c},"results":[{results}]}}"#),
            None => format!(r#"{{"results":[{results}]}}"#),
        }
    }

    /// A well-formed page yields one report per result with `(lon, lat)` read
    /// in coordinate order and obtime converted to epoch milliseconds. The
    /// lon/lat ordering is the critical guard: mPING `coordinates` are
    /// `[lon, lat]`, and a swap would relocate every report.
    #[wasm_bindgen_test]
    fn well_formed_page_parses_lon_lat_and_obtime() {
        let b = body(
            Some(2),
            &[
                result(10, "1970-01-01T00:00:01+00:00", "Hail", -97.5, 35.25),
                result(11, "1970-01-01T00:00:02+00:00", "Tornado", -98.0, 36.0),
            ],
        );
        let parsed = parse_response(&b).unwrap();
        assert_eq!(parsed.total_count, 2);
        assert_eq!(parsed.reports.len(), 2);

        let r0 = &parsed.reports[0];
        assert_eq!(r0.id, 10);
        assert_eq!(r0.category, ReportCategory::Hail);
        // lon = -97.5 (coords[0]), lat = 35.25 (coords[1]) — NOT swapped.
        assert!((r0.lon - (-97.5)).abs() < 1e-9);
        assert!((r0.lat - 35.25).abs() < 1e-9);
        // 1 second past the epoch == 1000 ms.
        assert!((r0.obtime_ms - 1000.0).abs() < 1e-9);

        assert_eq!(parsed.reports[1].category, ReportCategory::Tornado);
    }

    /// When `count` is absent, `total_count` falls back to the number of
    /// parsed results on this page.
    #[wasm_bindgen_test]
    fn missing_count_falls_back_to_results_len() {
        let b = body(
            None,
            &[result(1, "1970-01-01T00:00:01+00:00", "Flood", 0.0, 0.0)],
        );
        let parsed = parse_response(&b).unwrap();
        assert_eq!(parsed.total_count, 1);
    }

    /// A result missing its coordinates or obtime is silently skipped — not an
    /// error — so the surrounding good results still parse.
    #[wasm_bindgen_test]
    fn malformed_results_skipped_not_errored() {
        // Three results: good, missing geom, missing obtime.
        let good = result(1, "1970-01-01T00:00:01+00:00", "Hail", -97.0, 35.0);
        let no_geom =
            r#"{"id":2,"obtime":"1970-01-01T00:00:02+00:00","category":"Hail"}"#.to_string();
        let no_obtime =
            r#"{"id":3,"category":"Hail","geom":{"type":"Point","coordinates":[-98.0,36.0]}}"#
                .to_string();
        // count says 3 even though only one result is usable.
        let b = body(Some(3), &[good, no_geom, no_obtime]);
        let parsed = parse_response(&b).unwrap();
        // Only the good one survives, but total_count preserves the server's 3.
        assert_eq!(parsed.reports.len(), 1);
        assert_eq!(parsed.reports[0].id, 1);
        assert_eq!(parsed.total_count, 3);
    }

    /// A body with no `results` array is an error.
    #[wasm_bindgen_test]
    fn missing_results_array_errors() {
        assert!(parse_response(r#"{"count":0}"#).is_err());
    }

    /// An unknown category maps to `Other`.
    #[wasm_bindgen_test]
    fn unknown_category_maps_to_other() {
        let b = body(
            Some(1),
            &[result(1, "1970-01-01T00:00:01+00:00", "Frogs", 0.0, 0.0)],
        );
        let parsed = parse_response(&b).unwrap();
        assert_eq!(parsed.reports[0].category, ReportCategory::Other);
    }

    /// `ReportCategory::parse` round-trips its known strings, and the labels
    /// stay distinct.
    #[wasm_bindgen_test]
    fn report_category_parse_round_trips() {
        let cases = [
            ("Rain/Snow", ReportCategory::RainSnow),
            ("Hail", ReportCategory::Hail),
            ("Wind Damage", ReportCategory::WindDamage),
            ("Tornado", ReportCategory::Tornado),
            ("Flood", ReportCategory::Flood),
            ("Reduced Visibility", ReportCategory::ReducedVisibility),
        ];
        for (s, expected) in cases {
            assert_eq!(ReportCategory::parse(s), expected);
        }
        // Unknown → Other.
        assert_eq!(ReportCategory::parse("xyz"), ReportCategory::Other);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn one_result_body(inner: &str) -> String {
        format!(r#"{{"count":1,"results":[{inner}]}}"#)
    }

    #[wasm_bindgen_test]
    fn invalid_json_is_an_error() {
        assert!(parse_response("not json at all").is_err());
        assert!(parse_response("").is_err());
        assert!(parse_response("{").is_err());
    }

    #[wasm_bindgen_test]
    fn empty_results_array_is_ok_with_zero_reports() {
        let parsed = parse_response(r#"{"count":0,"results":[]}"#).unwrap();
        assert!(parsed.reports.is_empty());
        assert_eq!(parsed.total_count, 0);
        // Without count, falls back to results.len() == 0.
        let parsed = parse_response(r#"{"results":[]}"#).unwrap();
        assert_eq!(parsed.total_count, 0);
    }

    #[wasm_bindgen_test]
    fn report_without_integer_id_is_skipped() {
        // id absent.
        let b = one_result_body(
            r#"{"obtime":"1970-01-01T00:00:01+00:00","geom":{"coordinates":[1.0,2.0]}}"#,
        );
        assert!(parse_response(&b).unwrap().reports.is_empty());
        // id present but a string, not an integer.
        let b = one_result_body(
            r#"{"id":"x","obtime":"1970-01-01T00:00:01+00:00","geom":{"coordinates":[1.0,2.0]}}"#,
        );
        assert!(parse_response(&b).unwrap().reports.is_empty());
    }

    #[wasm_bindgen_test]
    fn coordinates_with_fewer_than_two_elements_are_skipped() {
        let b = one_result_body(
            r#"{"id":1,"obtime":"1970-01-01T00:00:01+00:00","geom":{"coordinates":[1.0]}}"#,
        );
        assert!(parse_response(&b).unwrap().reports.is_empty());
    }

    #[wasm_bindgen_test]
    fn invalid_obtime_is_skipped() {
        let b =
            one_result_body(r#"{"id":1,"obtime":"not-a-date","geom":{"coordinates":[1.0,2.0]}}"#);
        assert!(parse_response(&b).unwrap().reports.is_empty());
    }

    #[wasm_bindgen_test]
    fn missing_description_and_category_use_defaults() {
        // No description, no category fields — defaults to "" and Other.
        let b = one_result_body(
            r#"{"id":7,"obtime":"1970-01-01T00:00:01+00:00","geom":{"coordinates":[-97.0,35.0]}}"#,
        );
        let parsed = parse_response(&b).unwrap();
        assert_eq!(parsed.reports.len(), 1);
        let r = &parsed.reports[0];
        assert_eq!(r.id, 7);
        assert_eq!(r.description, "");
        assert_eq!(r.category, ReportCategory::Other);
    }

    #[wasm_bindgen_test]
    fn count_exceeding_page_results_is_preserved() {
        // Server reports 500 total but this page only carries one usable result.
        let b = format!(
            r#"{{"count":500,"results":[{}]}}"#,
            r#"{"id":1,"obtime":"1970-01-01T00:00:01+00:00","category":"Hail","geom":{"coordinates":[-97.0,35.0]}}"#
        );
        let parsed = parse_response(&b).unwrap();
        assert_eq!(parsed.reports.len(), 1);
        assert_eq!(parsed.total_count, 500);
    }
}
