//! Storm report types from the mPING API.

/// A single crowd-sourced storm report.
pub struct StormReport {
    /// Stable mPING report id (from `id` field of the API result).
    #[allow(dead_code)] // reserved for hover/click popovers
    pub id: i64,
    /// Observation time, milliseconds since Unix epoch.
    pub obtime_ms: f64,
    /// High-level category (`"Rain/Snow"`, `"Hail"`, `"Wind Damage"`,
    /// `"Tornado"`, `"Flood"`, `"Reduced Visibility"`, …).
    pub category: ReportCategory,
    /// Free-form description string from the API (e.g. "Mixed Ice Pellets and Snow").
    #[allow(dead_code)] // reserved for hover/click popovers
    pub description: String,
    /// Latitude in decimal degrees.
    pub lat: f64,
    /// Longitude in decimal degrees.
    pub lon: f64,
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
}
