//! GPU rendering: pixels out of decoded sweeps. Covers the WebGL2 radar
//! renderer with raw-to-physical shader conversion ([`gpu_renderer`]), the
//! 3D globe and volume ray-marching renderers ([`globe_radar_renderer`],
//! [`volume_ray_renderer`]), product color tables ([`color_table`]), the
//! national reflectivity mosaic overlay ([`national_mosaic`]), and the
//! coordinator that routes render requests to the worker pool
//! ([`render_coordinator`], [`render_request`]).

pub(crate) mod color_table;
pub(crate) mod globe_radar_renderer;
pub(crate) mod gpu_renderer;
pub(crate) mod national_mosaic;
pub(crate) mod render_coordinator;
pub(crate) mod render_request;
pub(crate) mod volume_ray_renderer;
