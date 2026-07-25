//! Shareable-view vocabulary — the URL `v` parameter blob.

use serde::{Deserialize, Serialize};

/// Opaque view-state blob encoded in the `v` URL parameter.
///
/// The URL encoding/decoding (and the `AppState` snapshot constructor
/// `ViewState::from_state`) live in the shell layer
/// (`src/state/url_state.rs`); this is just the wire shape, which may grow
/// over time without changing the URL schema.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ViewState {
    /// Map zoom level (f32).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mz: Option<f32>,
    /// Timeline zoom level (pixels per second).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz: Option<f64>,

    // ── 3D view parameters ──
    /// View mode: 0 = Flat2D, 1 = Globe3D.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm: Option<u8>,
    /// Camera mode: 0 = PlanetOrbit, 1 = SiteOrbit, 2 = FreeLook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cm: Option<u8>,
    /// Camera distance from globe center (Earth radii).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cd: Option<f32>,
    /// Camera center latitude (degrees) — planet orbit pivot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clat: Option<f32>,
    /// Camera center longitude (degrees) — planet orbit pivot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clon: Option<f32>,
    /// Camera tilt/pitch (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ct: Option<f32>,
    /// Camera rotation/yaw offset (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cr: Option<f32>,
    /// Site orbit bearing (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ob: Option<f32>,
    /// Site orbit elevation (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oe: Option<f32>,
    /// Free look position [x, y, z].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fp: Option<[f32; 3]>,
    /// Free look yaw (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fy: Option<f32>,
    /// Free look pitch (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fpt: Option<f32>,
    /// Free look speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs: Option<f32>,
    /// Volume 3D rendering enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v3d: Option<bool>,
    /// Volume density cutoff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vdc: Option<f32>,
    /// Real-time streaming active — when true, reloading re-enters live mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rt: Option<bool>,
}
