//! Layer visibility state.

/// State for toggling various overlay layers.
pub struct LayerState {
    /// Show NWS weather alerts overlay.
    pub nws_alerts: bool,

    /// Show historical tornado tracks.
    #[allow(dead_code)] // Future feature
    pub tornado_tracks: bool,

    /// Show political boundaries (state/county lines).
    #[allow(dead_code)] // Future feature
    pub political_boundaries: bool,

    /// Show terrain/topography.
    #[allow(dead_code)] // Future feature
    pub terrain: bool,

    /// Enable globe/3D mode
    pub globe_mode: bool,

    /// Enable multi-radar mosaic view
    pub multi_radar_mosaic: bool,

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
}

impl Default for LayerState {
    fn default() -> Self {
        Self {
            nws_alerts: true,
            tornado_tracks: false,
            political_boundaries: false,
            terrain: false,
            globe_mode: false,
            multi_radar_mosaic: false,
            geo: GeoLayerVisibility::default(),
        }
    }
}

impl Default for GeoLayerVisibility {
    fn default() -> Self {
        Self {
            states: true,
            counties: true,
            labels: true,
            nexrad_sites: false,
        }
    }
}
