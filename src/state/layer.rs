//! Layer visibility state.

use crate::geo::GeoLayerVisibility;

/// State for toggling various overlay layers.
#[derive(Default)]
pub struct LayerState {
    /// Geographic layer visibility settings
    pub geo: GeoLayerVisibility,
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn layer_state_default_wraps_geo_defaults() {
        let s = LayerState::default();
        assert!(s.geo.states);
        assert!(!s.geo.mping);
    }
}
