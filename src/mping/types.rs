//! Storm report types from the mPING API.

/// A single crowd-sourced storm report.
pub struct StormReport {
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
    pub fn visible_at(&self, playback_secs: f64) -> bool {
        self.obtime_ms <= playback_secs * 1000.0
    }
}

/// Coarse category bucket used for color coding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportCategory {
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
    pub fn parse(s: &str) -> Self {
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
    pub fn label(self) -> &'static str {
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
