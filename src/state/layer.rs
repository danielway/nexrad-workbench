//! Layer visibility state.

/// State for toggling various overlay layers.
#[derive(Default)]
pub struct LayerState {
    /// Show NWS weather alerts overlay
    pub nws_alerts: bool,

    /// Show historical tornado tracks
    pub tornado_tracks: bool,

    /// Show political boundaries (state/county lines)
    pub political_boundaries: bool,

    /// Show terrain/topography
    pub terrain: bool,

    /// Enable globe/3D mode
    pub globe_mode: bool,

    /// Enable multi-radar mosaic view
    pub multi_radar_mosaic: bool,
}
