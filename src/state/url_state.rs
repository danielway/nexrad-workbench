//! URL state encoding/decoding for shareable URLs.
//!
//! Encodes site, playback time, product, and map center in the URL query string
//! so reloading restores the view and URLs can be shared.
//!
//! The `v` parameter is an opaque base64-encoded JSON blob carrying
//! auxiliary view state (map zoom, timeline zoom, etc.) that may grow
//! over time without changing the URL schema.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Opaque view-state blob encoded in the `v` URL parameter.
#[derive(Default, Serialize, Deserialize)]
pub struct ViewState {
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
}

/// Parsed URL parameters.
pub struct UrlParams {
    pub site: Option<String>,
    pub time: Option<f64>,
    pub product: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub view: ViewState,
}

/// Parse URL query parameters from the current browser URL.
pub fn parse_from_url() -> UrlParams {
    let mut params = UrlParams {
        site: None,
        time: None,
        product: None,
        lat: None,
        lon: None,
        view: ViewState::default(),
    };

    let Ok(search) = web_sys::window().expect("no window").location().search() else {
        return params;
    };

    let query = search.trim_start_matches('?');
    if query.is_empty() {
        return params;
    }

    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        match key {
            "site" => params.site = Some(value.to_string()),
            "t" => params.time = value.parse().ok(),
            "product" => params.product = Some(value.to_string()),
            "lat" => params.lat = value.parse().ok(),
            "lon" => params.lon = value.parse().ok(),
            "v" => {
                if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value) {
                    if let Ok(vs) = serde_json::from_slice::<ViewState>(&bytes) {
                        params.view = vs;
                    }
                }
            }
            _ => {}
        }
    }

    params
}

/// Push current state to the URL query string using `replaceState`.
pub fn push_to_url(site: &str, time: f64, product: &str, lat: f64, lon: f64, view: &ViewState) {
    let v_json = serde_json::to_vec(view).unwrap_or_default();
    let v_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&v_json);

    let query = format!(
        "?site={}&t={:.0}&product={}&lat={:.4}&lon={:.4}&v={}",
        site, time, product, lat, lon, v_b64
    );

    let window = web_sys::window().expect("no window");
    let history = window.history().expect("no history");
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&query));
}
