//! Geographic layer system for map overlays.
//!
//! This module provides functionality for loading and rendering geographic
//! features such as state boundaries, county lines, and city markers.

pub(crate) mod camera;
pub(crate) mod cities;
pub(crate) mod geo_line_renderer;
pub(crate) mod globe_renderer;
mod layer;
mod projection;
mod renderer;

#[allow(unused_imports)] // GlobeProjection is the 3D Projection adapter.
pub(crate) use camera::GlobeProjection;
pub(crate) use camera::{Camera, CameraMode, Flat2DState, UrlCameraSnapshot, ViewMode};
pub(crate) use geo_line_renderer::GeoLineRenderer;
pub(crate) use globe_renderer::GlobeRenderer;
pub(crate) use layer::{GeoFeature, GeoLayer, GeoLayerSet, GeoLayerType, GeoLayerVisibility};
pub(crate) use projection::{MapProjection, Projection, ProjectionFingerprint};
pub(crate) use renderer::{render_geo_layers, text_with_halo, GeoPass};
