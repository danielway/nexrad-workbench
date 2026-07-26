//! The map-overlay layers the user can switch on and off, as a value.
//!
//! The right panel's "Layers" list used to write
//! `state.layer_state.geo.<field>` straight through an `&mut bool` checkbox
//! binding — one direct mutation per layer. Naming the layer instead lets the
//! whole list emit one intent ([`crate::core::Intent::SetGeoLayer`]) and keeps
//! the field mapping here, in the core, where it is testable.
//!
//! [`GeoLayerVisibility`] itself stays in `geo` (it is the renderer's input);
//! this enum is the addressing vocabulary over it.

use crate::geo::GeoLayerVisibility;

/// A user-switchable map overlay.
///
/// Only the layers with a real toggle surface are listed. `highways` / `lakes`
/// exist on [`GeoLayerVisibility`] but have no control today, so naming them
/// here would be dead vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeoLayer {
    /// Other NEXRAD site markers (not the active site).
    NexradSites,
    /// State / province boundaries.
    States,
    /// County boundaries.
    Counties,
    /// Major cities.
    Cities,
    /// Labels for geographic features.
    Labels,
    /// The CONUS base-reflectivity composite overlay.
    NationalMosaic,
    /// NWS warning polygons (the urgent, storm-based alerts).
    AlertsWarnings,
    /// NWS watch / advisory / statement polygons.
    AlertsOther,
    /// mPING crowd-sourced storm reports.
    Mping,
    /// The user's device location dot. Per-session only; enabling it also
    /// kicks off the one-shot geolocation lookup (a diagnostics intent).
    GpsLocation,
}

impl GeoLayer {
    /// Read this layer's current visibility.
    pub(crate) fn get(self, v: &GeoLayerVisibility) -> bool {
        match self {
            GeoLayer::NexradSites => v.nexrad_sites,
            GeoLayer::States => v.states,
            GeoLayer::Counties => v.counties,
            GeoLayer::Cities => v.cities,
            GeoLayer::Labels => v.labels,
            GeoLayer::NationalMosaic => v.national_mosaic,
            GeoLayer::AlertsWarnings => v.alerts_warnings,
            GeoLayer::AlertsOther => v.alerts_other,
            GeoLayer::Mping => v.mping,
            GeoLayer::GpsLocation => v.gps_location,
        }
    }

    /// Set this layer's visibility.
    pub(crate) fn set(self, v: &mut GeoLayerVisibility, on: bool) {
        match self {
            GeoLayer::NexradSites => v.nexrad_sites = on,
            GeoLayer::States => v.states = on,
            GeoLayer::Counties => v.counties = on,
            GeoLayer::Cities => v.cities = on,
            GeoLayer::Labels => v.labels = on,
            GeoLayer::NationalMosaic => v.national_mosaic = on,
            GeoLayer::AlertsWarnings => v.alerts_warnings = on,
            GeoLayer::AlertsOther => v.alerts_other = on,
            GeoLayer::Mping => v.mping = on,
            GeoLayer::GpsLocation => v.gps_location = on,
        }
    }

    /// Every toggleable layer, for exhaustive tests.
    #[cfg(test)]
    pub(crate) fn all() -> [GeoLayer; 10] {
        [
            GeoLayer::NexradSites,
            GeoLayer::States,
            GeoLayer::Counties,
            GeoLayer::Cities,
            GeoLayer::Labels,
            GeoLayer::NationalMosaic,
            GeoLayer::AlertsWarnings,
            GeoLayer::AlertsOther,
            GeoLayer::Mping,
            GeoLayer::GpsLocation,
        ]
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn set_then_get_round_trips_for_every_layer() {
        let mut v = GeoLayerVisibility::default();
        for layer in GeoLayer::all() {
            layer.set(&mut v, true);
            assert!(layer.get(&v), "{:?} should read back true", layer);
            layer.set(&mut v, false);
            assert!(!layer.get(&v), "{:?} should read back false", layer);
        }
    }

    #[wasm_bindgen_test]
    fn each_layer_addresses_a_distinct_field() {
        // Flip one layer on from an all-off baseline and confirm no sibling moved.
        for target in GeoLayer::all() {
            let mut v = GeoLayerVisibility::default();
            for l in GeoLayer::all() {
                l.set(&mut v, false);
            }
            target.set(&mut v, true);
            for l in GeoLayer::all() {
                assert_eq!(
                    l.get(&v),
                    l == target,
                    "setting {:?} disturbed {:?}",
                    target,
                    l
                );
            }
        }
    }

    #[wasm_bindgen_test]
    fn get_reflects_the_shipped_defaults() {
        let v = GeoLayerVisibility::default();
        assert!(GeoLayer::States.get(&v));
        assert!(GeoLayer::Counties.get(&v));
        assert!(GeoLayer::Cities.get(&v));
        assert!(GeoLayer::Labels.get(&v));
        assert!(GeoLayer::AlertsWarnings.get(&v));
        assert!(!GeoLayer::NexradSites.get(&v));
        assert!(!GeoLayer::NationalMosaic.get(&v));
        assert!(!GeoLayer::AlertsOther.get(&v));
        assert!(!GeoLayer::Mping.get(&v));
        assert!(!GeoLayer::GpsLocation.get(&v));
    }
}
