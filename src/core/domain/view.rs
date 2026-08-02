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
    /// LEGACY, parse-only: camera mode of pre-overhaul links
    /// (0 = PlanetOrbit, 1 = SiteOrbit, 2 = FreeLook). Never written; the
    /// unified camera maps it in `Camera::restore_from_url_fields`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cm: Option<u8>,
    /// Camera distance from globe center (Earth radii).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cd: Option<f32>,
    /// Camera pivot latitude (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clat: Option<f32>,
    /// Camera pivot longitude (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clon: Option<f32>,
    /// Camera tilt off vertical (degrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ct: Option<f32>,
    /// Camera heading (degrees, 0 = north up).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cr: Option<f32>,
    /// LEGACY, parse-only: site-orbit bearing (degrees). Never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ob: Option<f32>,
    /// LEGACY, parse-only: site-orbit elevation (degrees). Never written.
    /// (The free-look fields `fp`/`fy`/`fpt`/`fs` were dropped entirely —
    /// serde ignores unknown keys, so old blobs still parse.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oe: Option<f32>,
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
