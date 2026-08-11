//! Browser location I/O: geolocation and zip-code geocoding.
//!
//! Both are executed here, on the shell side, as
//! [`Effect::StartGeolocation`](crate::core::Effect::StartGeolocation) /
//! [`LocateForSite`](crate::core::Effect::LocateForSite) /
//! [`GeocodeZip`](crate::core::Effect::GeocodeZip). Results are delivered
//! asynchronously through a [`LocationResult`] channel the caller supplies —
//! the GPS overlay's `GpsState` channel for the overlay lookup, the site
//! modal's own channel for site selection. The UI never touches `web_sys` or
//! spawns these futures itself.

use crate::core::{LocationResult, LocationSource};
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use crate::WorkbenchApp;
use eframe::egui;
use futures_channel::mpsc::UnboundedSender;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

impl WorkbenchApp {
    /// Open the site modal in its pending state and start a geolocation
    /// lookup for site selection. Shared by the modal's "Use My Location"
    /// button and the mobile action bar's location button, so both paths
    /// present identically.
    pub(crate) fn begin_site_geolocation(&mut self, ctx: &egui::Context) {
        self.chrome.site_modal_open = true;
        self.modals.site.begin_pending();
        start_geolocation(self.modals.site.location_sender(), ctx.clone());
    }

    /// Validate a raw zip submission and either start the lookup or show the
    /// validation message. The decision itself is pure
    /// ([`crate::core::geocode::decide_zip_submission`]).
    pub(crate) fn begin_site_zip_lookup(&mut self, ctx: &egui::Context, raw: String) {
        match crate::core::geocode::decide_zip_submission(&raw) {
            Ok(zip) => {
                self.modals.site.begin_pending();
                start_zip_lookup(&zip, self.modals.site.location_sender(), ctx.clone());
            }
            Err(message) => self.modals.site.show_error(message.to_string()),
        }
    }
}

/// Start browser geolocation lookup.
///
/// `results` is an unbounded mpsc sender that the success/error callbacks
/// push their outcome into. The caller is responsible for draining the
/// corresponding receiver each frame.
pub(super) fn start_geolocation(
    results: futures_channel::mpsc::UnboundedSender<LocationResult>,
    ctx: egui::Context,
) {
    let window = match web_sys::window() {
        Some(w) => w,
        None => {
            let _ = results.unbounded_send(LocationResult::Error("No browser window".into()));
            return;
        }
    };

    let navigator = window.navigator();
    let geolocation = match navigator.geolocation() {
        Ok(g) => g,
        Err(_) => {
            let _ =
                results.unbounded_send(LocationResult::Error("Geolocation not available".into()));
            return;
        }
    };

    let results_ok = results.clone();
    let ctx_ok = ctx.clone();
    let success_cb = Closure::once(move |position: JsValue| {
        let coords = js_sys::Reflect::get(&position, &"coords".into()).unwrap();
        let lat = js_sys::Reflect::get(&coords, &"latitude".into())
            .unwrap()
            .as_f64()
            .unwrap_or(0.0);
        let lon = js_sys::Reflect::get(&coords, &"longitude".into())
            .unwrap()
            .as_f64()
            .unwrap_or(0.0);
        let _ = results_ok.unbounded_send(LocationResult::Success {
            lat,
            lon,
            source: LocationSource::Geolocation,
        });
        ctx_ok.request_repaint();
    });

    let results_err = results;
    let ctx_err = ctx;
    let error_cb = Closure::once(move |error: JsValue| {
        let msg = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "Location access denied".into());
        let _ = results_err.unbounded_send(LocationResult::Error(msg));
        ctx_err.request_repaint();
    });

    let _ = geolocation.get_current_position_with_error_callback(
        success_cb.as_ref().unchecked_ref(),
        Some(error_cb.as_ref().unchecked_ref()),
    );

    // Prevent closures from being dropped (they need to live until the callback fires).
    success_cb.forget();
    error_cb.forget();
}

/// Start zip code geocoding via the Zippopotam.us API.
fn start_zip_lookup(zip: &str, results: UnboundedSender<LocationResult>, ctx: egui::Context) {
    let url = format!("https://api.zippopotam.us/us/{}", zip);

    wasm_bindgen_futures::spawn_local(async move {
        let result: Result<(f64, f64), String> =
            with_retry(&DEFAULT_POLICY, "zip_lookup", |_attempt| {
                let url = url.clone();
                async move { zip_lookup_attempt(&url).await }
            })
            .await
            .map_err(|msg| {
                // Zippopotam returns 404 for invalid zips; surface a friendlier
                // message than the raw HTTP status.
                if msg.contains("HTTP 404") {
                    "Zip code not found".to_string()
                } else {
                    msg
                }
            });

        let payload = match result {
            Ok((lat, lon)) => LocationResult::Success {
                lat,
                lon,
                source: LocationSource::Zip,
            },
            Err(e) => LocationResult::Error(e),
        };
        let _ = results.unbounded_send(payload);
        ctx.request_repaint();
    });
}

/// One attempt against the Zippopotam.us API. Network errors and 5xx are
/// retryable; 404 (invalid zip) and parse failures are terminal.
async fn zip_lookup_attempt(url: &str) -> Verdict<(f64, f64)> {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return Verdict::Terminal("No browser window".into()),
    };

    let resp_value = match wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await {
        Ok(v) => v,
        Err(_) => return Verdict::Retry { after: None },
    };
    let resp: web_sys::Response = match resp_value.dyn_into() {
        Ok(r) => r,
        Err(_) => return Verdict::Terminal("Invalid response".into()),
    };

    let status = resp.status();
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        return Verdict::Retry { after: None };
    }
    if !resp.ok() {
        return Verdict::Terminal(format!("HTTP {}", status));
    }

    let json_promise = match resp.json() {
        Ok(p) => p,
        Err(_) => return Verdict::Terminal("Failed to parse response".into()),
    };
    let json = match wasm_bindgen_futures::JsFuture::from(json_promise).await {
        Ok(v) => v,
        Err(_) => return Verdict::Retry { after: None },
    };

    // Zippopotam response: { "places": [{ "latitude": "...", "longitude": "..." }] }
    let places = match js_sys::Reflect::get(&json, &"places".into()) {
        Ok(p) => p,
        Err(_) => return Verdict::Terminal("Invalid response format".into()),
    };
    let first = match js_sys::Reflect::get_u32(&places, 0) {
        Ok(f) => f,
        Err(_) => return Verdict::Terminal("No location data for zip code".into()),
    };

    let lat_str = match js_sys::Reflect::get(&first, &"latitude".into()) {
        Ok(v) => match v.as_string() {
            Some(s) => s,
            None => return Verdict::Terminal("Invalid latitude".into()),
        },
        Err(_) => return Verdict::Terminal("Missing latitude".into()),
    };
    let lon_str = match js_sys::Reflect::get(&first, &"longitude".into()) {
        Ok(v) => match v.as_string() {
            Some(s) => s,
            None => return Verdict::Terminal("Invalid longitude".into()),
        },
        Err(_) => return Verdict::Terminal("Missing longitude".into()),
    };

    let lat: f64 = match lat_str.parse() {
        Ok(v) => v,
        Err(_) => return Verdict::Terminal("Invalid latitude value".into()),
    };
    let lon: f64 = match lon_str.parse() {
        Ok(v) => v,
        Err(_) => return Verdict::Terminal("Invalid longitude value".into()),
    };

    Verdict::Ok((lat, lon))
}
