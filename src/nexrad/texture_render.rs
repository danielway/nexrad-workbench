//! NEXRAD rendering constants.

/// Returns the standard NEXRAD coverage range in km.
///
/// Standard NEXRAD range is approximately 230km for base reflectivity
/// and up to 460km for long-range products.
pub fn radar_coverage_range_km() -> f64 {
    300.0
}
