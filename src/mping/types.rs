//! Storm report types from the mPING API.

/// A single crowd-sourced storm report.
pub(crate) struct StormReport {
    /// Stable mPING report id (from `id` field of the API result).
    pub id: i64,
    /// Observation time, milliseconds since Unix epoch.
    pub obtime_ms: f64,
    /// High-level category (`"Rain/Snow"`, `"Hail"`, `"Wind Damage"`,
    /// `"Tornado"`, `"Flood"`, `"Reduced Visibility"`, …).
    pub category: ReportCategory,
    /// Free-form description string from the API (e.g. "Mixed Ice Pellets and Snow").
    pub description: String,
    /// Latitude in decimal degrees.
    pub lat: f64,
    /// Longitude in decimal degrees.
    pub lon: f64,
}

impl StormReport {
    /// Whether this report should be visible at the given playback
    /// position (Unix seconds). We never show a report observed *after*
    /// the time being rendered — surfacing a future report would be a
    /// lie about what was known at that moment.
    pub(crate) fn visible_at(&self, playback_secs: f64) -> bool {
        self.obtime_ms <= playback_secs * 1000.0
    }
}

/// Coarse category bucket used for color coding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportCategory {
    RainSnow,
    Hail,
    WindDamage,
    Tornado,
    Flood,
    ReducedVisibility,
    Other,
}

impl ReportCategory {
    /// Map an mPING `category` string to a category bucket.
    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "Rain/Snow" => Self::RainSnow,
            "Hail" => Self::Hail,
            "Wind Damage" => Self::WindDamage,
            "Tornado" => Self::Tornado,
            "Flood" => Self::Flood,
            "Reduced Visibility" => Self::ReducedVisibility,
            _ => Self::Other,
        }
    }

    /// Human-readable label for the category, used in detail popovers.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RainSnow => "Rain / Snow",
            Self::Hail => "Hail",
            Self::WindDamage => "Wind Damage",
            Self::Tornado => "Tornado",
            Self::Flood => "Flood",
            Self::ReducedVisibility => "Reduced Visibility",
            Self::Other => "Other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn report_at(obtime_ms: f64) -> StormReport {
        StormReport {
            id: 1,
            obtime_ms,
            category: ReportCategory::Other,
            description: String::new(),
            lat: 0.0,
            lon: 0.0,
        }
    }

    #[wasm_bindgen_test]
    fn visible_at_boundary() {
        // playback at 1000 s == 1_000_000 ms.
        let p = 1000.0;
        // Exactly at the playhead is visible.
        assert!(report_at(1_000_000.0).visible_at(p));
        // One ms before is visible.
        assert!(report_at(999_999.0).visible_at(p));
        // One ms after (the future) is hidden.
        assert!(!report_at(1_000_001.0).visible_at(p));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn report_at(obtime_ms: f64) -> StormReport {
        StormReport {
            id: 1,
            obtime_ms,
            category: ReportCategory::Other,
            description: String::new(),
            lat: 0.0,
            lon: 0.0,
        }
    }

    #[wasm_bindgen_test]
    fn parse_maps_known_categories() {
        assert_eq!(ReportCategory::parse("Rain/Snow"), ReportCategory::RainSnow);
        assert_eq!(ReportCategory::parse("Hail"), ReportCategory::Hail);
        assert_eq!(
            ReportCategory::parse("Wind Damage"),
            ReportCategory::WindDamage
        );
        assert_eq!(ReportCategory::parse("Tornado"), ReportCategory::Tornado);
        assert_eq!(ReportCategory::parse("Flood"), ReportCategory::Flood);
        assert_eq!(
            ReportCategory::parse("Reduced Visibility"),
            ReportCategory::ReducedVisibility
        );
    }

    #[wasm_bindgen_test]
    fn parse_unknown_and_empty_fall_back_to_other() {
        assert_eq!(ReportCategory::parse(""), ReportCategory::Other);
        assert_eq!(ReportCategory::parse("Snow/Rain"), ReportCategory::Other);
        assert_eq!(ReportCategory::parse("rain/snow"), ReportCategory::Other);
        assert_eq!(ReportCategory::parse(" Hail "), ReportCategory::Other);
        assert_eq!(ReportCategory::parse("Other"), ReportCategory::Other);
    }

    #[wasm_bindgen_test]
    fn parse_is_case_sensitive() {
        // Exact-match only: differing case must not match.
        assert_eq!(ReportCategory::parse("HAIL"), ReportCategory::Other);
        assert_eq!(ReportCategory::parse("tornado"), ReportCategory::Other);
        assert_eq!(ReportCategory::parse("wind damage"), ReportCategory::Other);
    }

    #[wasm_bindgen_test]
    fn label_matches_each_variant() {
        assert_eq!(ReportCategory::RainSnow.label(), "Rain / Snow");
        assert_eq!(ReportCategory::Hail.label(), "Hail");
        assert_eq!(ReportCategory::WindDamage.label(), "Wind Damage");
        assert_eq!(ReportCategory::Tornado.label(), "Tornado");
        assert_eq!(ReportCategory::Flood.label(), "Flood");
        assert_eq!(
            ReportCategory::ReducedVisibility.label(),
            "Reduced Visibility"
        );
        assert_eq!(ReportCategory::Other.label(), "Other");
    }

    #[wasm_bindgen_test]
    fn parse_label_roundtrip_for_unambiguous_variants() {
        // For categories whose API string equals their label, parsing the
        // label yields the same variant. (RainSnow's label has spaces, so
        // it is intentionally excluded.)
        assert_eq!(
            ReportCategory::parse(ReportCategory::Hail.label()),
            ReportCategory::Hail
        );
        assert_eq!(
            ReportCategory::parse(ReportCategory::WindDamage.label()),
            ReportCategory::WindDamage
        );
        assert_eq!(
            ReportCategory::parse(ReportCategory::Tornado.label()),
            ReportCategory::Tornado
        );
        assert_eq!(
            ReportCategory::parse(ReportCategory::Flood.label()),
            ReportCategory::Flood
        );
        assert_eq!(
            ReportCategory::parse(ReportCategory::ReducedVisibility.label()),
            ReportCategory::ReducedVisibility
        );
    }

    #[wasm_bindgen_test]
    fn visible_at_zero_and_origin() {
        // A report observed at epoch (obtime 0) is visible at playback 0.
        assert!(report_at(0.0).visible_at(0.0));
        // Any positive observation time is in the future relative to playback 0.
        assert!(!report_at(1.0).visible_at(0.0));
    }

    #[wasm_bindgen_test]
    fn visible_at_negative_and_large_values() {
        // Negative observation time (pre-epoch) is always in the past.
        assert!(report_at(-5000.0).visible_at(0.0));
        assert!(report_at(-5000.0).visible_at(-1.0));
        // A report far in the future is hidden until playback reaches it.
        // obtime 2_000_000 ms == playback 2000 s exactly is visible.
        assert!(report_at(2_000_000.0).visible_at(2000.0));
        assert!(!report_at(2_000_000.0).visible_at(1999.0));
    }
}
