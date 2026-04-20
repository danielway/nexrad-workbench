//! NWS alerts API fetch logic.
//!
//! Uses the browser Fetch API via web-sys. The endpoint is CORS-enabled
//! and requires no authentication. We send `If-None-Match` with the last
//! seen ETag to let the server return 304 when nothing has changed.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::channel::{AlertsChannel, AlertsEvent};
use super::parse::parse_response;

/// Endpoint for currently-active alerts across the US. The API returns
/// GeoJSON; the `Accept` header requests the weather.gov content type.
const ALERTS_URL: &str = "https://api.weather.gov/alerts/active";
const ACCEPT: &str = "application/geo+json";
/// Browsers will usually ignore or overwrite this, but the NWS API
/// recommends an identifying value. We set it best-effort.
const USER_AGENT: &str = "NEXRAD-Workbench (https://github.com/danielway/nexrad-workbench)";

/// Spawn a background fetch. Results are pushed into `channel` when done.
pub fn spawn_fetch(
    ctx: eframe::egui::Context,
    channel: AlertsChannel,
    if_none_match: Option<String>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let event = match fetch_inner(if_none_match).await {
            Ok(FetchOutcome::Updated { body, etag }) => match parse_response(&body) {
                Ok(parsed) => AlertsEvent::Updated {
                    alerts: parsed.alerts,
                    etag,
                },
                Err(e) => AlertsEvent::Error(format!("parse failed: {}", e)),
            },
            Ok(FetchOutcome::NotModified) => AlertsEvent::NotModified,
            Err(e) => AlertsEvent::Error(e),
        };
        channel.push(event);
        ctx.request_repaint();
    });
}

enum FetchOutcome {
    Updated { body: String, etag: Option<String> },
    NotModified,
}

async fn fetch_inner(if_none_match: Option<String>) -> Result<FetchOutcome, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;

    // Build a Request with the custom headers we need.
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);

    let headers = web_sys::Headers::new().map_err(|_| "failed to allocate headers".to_string())?;
    let _ = headers.set("Accept", ACCEPT);
    let _ = headers.set("User-Agent", USER_AGENT);
    if let Some(etag) = if_none_match.as_ref() {
        let _ = headers.set("If-None-Match", etag);
    }
    init.set_headers(&JsValue::from(headers));

    let request = web_sys::Request::new_with_str_and_init(ALERTS_URL, &init)
        .map_err(|e| format!("request init failed: {:?}", e))?;

    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("network error: {}", err_text(e)))?;

    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|_| "invalid response object".to_string())?;

    let status = resp.status();
    if status == 304 {
        return Ok(FetchOutcome::NotModified);
    }
    if !resp.ok() {
        return Err(format!("HTTP {}", status));
    }

    let etag = resp.headers().get("ETag").ok().flatten();

    let text_promise = resp
        .text()
        .map_err(|e| format!("failed to read body: {}", err_text(e)))?;
    let text_value = JsFuture::from(text_promise)
        .await
        .map_err(|e| format!("failed to read body: {}", err_text(e)))?;
    let body = text_value
        .as_string()
        .ok_or_else(|| "body not a string".to_string())?;

    Ok(FetchOutcome::Updated { body, etag })
}

fn err_text(v: JsValue) -> String {
    v.as_string()
        .or_else(|| {
            js_sys::Reflect::get(&v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{:?}", v))
}
