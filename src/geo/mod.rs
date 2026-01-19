//! Geographic layer system for map overlays.
//!
//! This module provides functionality for loading and rendering geographic
//! features such as state boundaries, county lines, rivers, cities, etc.

mod layer;
mod projection;
mod renderer;

pub use layer::{GeoFeature, GeoLayer, GeoLayerSet, GeoLayerType};
pub use projection::MapProjection;
pub use renderer::render_geo_layers;
