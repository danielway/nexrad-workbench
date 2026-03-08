//! Geographic layer system for map overlays.
//!
//! This module provides functionality for loading and rendering geographic
//! features such as state boundaries, county lines, and city markers.

pub mod camera;
pub(crate) mod cities;
pub mod geo_line_renderer;
pub mod globe_renderer;
mod layer;
mod projection;
mod renderer;

pub use camera::GlobeCamera;
pub use geo_line_renderer::GeoLineRenderer;
pub use globe_renderer::GlobeRenderer;
pub use layer::{GeoFeature, GeoLayer, GeoLayerSet, GeoLayerType};
pub use projection::MapProjection;
pub use renderer::render_geo_layers;
