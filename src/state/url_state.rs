//! URL state encoding/decoding for shareable URLs.
//!
//! Encodes site, playback time, and map center in the URL query string
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
}

/// Parsed URL parameters.
pub struct UrlParams {
    pub site: Option<String>,
    pub time: Option<f64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub view: ViewState,
}

/// Parse URL query parameters from the current browser URL.
pub fn parse_from_url() -> UrlParams {
    let mut params = UrlParams {
        site: None,
        time: None,
        lat: None,
        lon: None,
        view: ViewState::default(),
    };

    let Ok(search) = web_sys::window()
        .expect("no window")
        .location()
        .search()
    else {
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
pub fn push_to_url(site: &str, time: f64, lat: f64, lon: f64, view: &ViewState) {
    let v_json = serde_json::to_vec(view).unwrap_or_default();
    let v_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&v_json);

    let query = format!(
        "?site={}&t={:.0}&lat={:.4}&lon={:.4}&v={}",
        site, time, lat, lon, v_b64
    );

    let window = web_sys::window().expect("no window");
    let history = window.history().expect("no history");
    let _ = history.replace_state_with_url(
        &wasm_bindgen::JsValue::NULL,
        "",
        Some(&query),
    );
}
