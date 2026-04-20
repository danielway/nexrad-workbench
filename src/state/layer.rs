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
    /// Show NWS active alert polygons
    pub alerts: bool,
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
            alerts: false,
        }
    }
}
