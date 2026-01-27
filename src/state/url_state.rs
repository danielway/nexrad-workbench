//! URL state encoding/decoding for shareable URLs.
//!
//! Encodes site, playback time, and map center in the URL query string
//! so reloading restores the view and URLs can be shared.

/// Parsed URL parameters.
pub struct UrlParams {
    pub site: Option<String>,
    pub time: Option<f64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Parse URL query parameters from the current browser URL.
#[cfg(target_arch = "wasm32")]
pub fn parse_from_url() -> UrlParams {
    let mut params = UrlParams {
        site: None,
        time: None,
        lat: None,
        lon: None,
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
            _ => {}
        }
    }

    params
}

/// No-op stub for native builds.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_from_url() -> UrlParams {
    UrlParams {
        site: None,
        time: None,
        lat: None,
        lon: None,
    }
}

/// Push current state to the URL query string using `replaceState`.
#[cfg(target_arch = "wasm32")]
pub fn push_to_url(site: &str, time: f64, lat: f64, lon: f64) {
    let query = format!(
        "?site={}&t={:.0}&lat={:.4}&lon={:.4}",
        site, time, lat, lon
    );

    let window = web_sys::window().expect("no window");
    let history = window.history().expect("no history");
    let _ = history.replace_state_with_url(
        &wasm_bindgen::JsValue::NULL,
        "",
        Some(&query),
    );
}

/// No-op stub for native builds.
#[cfg(not(target_arch = "wasm32"))]
pub fn push_to_url(_site: &str, _time: f64, _lat: f64, _lon: f64) {}
