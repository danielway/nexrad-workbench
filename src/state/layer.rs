//! Layer visibility state.

/// State for toggling various overlay layers.
#[derive(Default)]
pub struct LayerState {
    /// Geographic layer visibility settings
    pub geo: GeoLayerVisibility,
}

/// Visibility settings for geographic map layers.
#[derive(Clone)]
pub struct GeoLayerVisibility {
    /// Show state/province boundaries
    pub states: bool,
    /// Show county boundaries (auto-hidden at low zoom)
    pub counties: bool,
    /// Show labels for geographic features
    pub labels: bool,
    /// Show NEXRAD radar sites (other sites, not current)
    pub nexrad_sites: bool,
    /// Show major cities
    pub cities: bool,
    /// Show major highways
    pub highways: bool,
    /// Show lakes and water bodies
    pub lakes: bool,
    /// Show the national radar mosaic overlay (CONUS composite)
    pub national_mosaic: bool,
    /// Show NWS warning polygons (the urgent, storm-based alerts)
    pub alerts_warnings: bool,
    /// Show NWS watch/advisory/statement polygons (everything that isn't a warning)
    pub alerts_other: bool,
    /// Show mPING crowd-sourced storm reports
    pub mping: bool,
    /// Show the user's current GPS location as a dot on the map.
    /// Per-session only — not persisted to UserPreferences.
    pub gps_location: bool,
}

impl Default for GeoLayerVisibility {
    fn default() -> Self {
        Self {
            states: true,
            counties: true,
            labels: true,
            nexrad_sites: false,
            cities: true,
            highways: false,
            lakes: false,
            national_mosaic: false,
            alerts_warnings: true,
            alerts_other: false,
            mping: false,
            gps_location: false,
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_geo_visibility_matches_product_defaults() {
        // Pins the out-of-the-box overlay set: base geography + warnings on,
        // opt-in/heavier overlays off.
        let v = GeoLayerVisibility::default();
        assert!(v.states);
        assert!(v.counties);
        assert!(v.labels);
        assert!(v.cities);
        assert!(v.alerts_warnings);

        assert!(!v.nexrad_sites);
        assert!(!v.highways);
        assert!(!v.lakes);
        assert!(!v.national_mosaic);
        assert!(!v.alerts_other);
        assert!(!v.mping);
        assert!(!v.gps_location);
    }

    #[wasm_bindgen_test]
    fn layer_state_default_wraps_geo_defaults() {
        let s = LayerState::default();
        assert!(s.geo.states);
        assert!(!s.geo.mping);
    }
}
