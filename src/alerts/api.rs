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
use crate::net::retry::{parse_retry_after, with_retry, Verdict, DEFAULT_POLICY};

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
    with_retry(&DEFAULT_POLICY, "alerts", |_attempt| {
        let etag = if_none_match.clone();
        async move { fetch_attempt(etag).await }
    })
    .await
}

/// Run a single fetch attempt and classify its outcome.
async fn fetch_attempt(if_none_match: Option<String>) -> Verdict<FetchOutcome> {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return Verdict::Terminal("no window".into()),
    };

    // Build a Request with the custom headers we need.
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);

    let headers = match web_sys::Headers::new() {
        Ok(h) => h,
        Err(_) => return Verdict::Terminal("failed to allocate headers".into()),
    };
    let _ = headers.set("Accept", ACCEPT);
    let _ = headers.set("User-Agent", USER_AGENT);
    if let Some(etag) = if_none_match.as_ref() {
        let _ = headers.set("If-None-Match", etag);
    }
    init.set_headers(&JsValue::from(headers));

    let request = match web_sys::Request::new_with_str_and_init(ALERTS_URL, &init) {
        Ok(r) => r,
        Err(e) => return Verdict::Terminal(format!("request init failed: {:?}", e)),
    };

    // Network-layer error (DNS, connection refused, CORS preflight failure, …)
    // is retryable.
    let resp_value = match JsFuture::from(window.fetch_with_request(&request)).await {
        Ok(v) => v,
        Err(e) => {
            log::debug!("alerts: network error: {}", err_text(e));
            return Verdict::Retry { after: None };
        }
    };

    let resp: web_sys::Response = match resp_value.dyn_into() {
        Ok(r) => r,
        Err(_) => return Verdict::Terminal("invalid response object".into()),
    };

    let status = resp.status();
    if status == 304 {
        return Verdict::Ok(FetchOutcome::NotModified);
    }
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        let after = resp
            .headers()
            .get("Retry-After")
            .ok()
            .flatten()
            .and_then(|s| parse_retry_after(&s));
        return Verdict::Retry { after };
    }
    if !resp.ok() {
        return Verdict::Terminal(format!("HTTP {}", status));
    }

    let etag = resp.headers().get("ETag").ok().flatten();

    let text_promise = match resp.text() {
        Ok(p) => p,
        Err(e) => return Verdict::Terminal(format!("failed to read body: {}", err_text(e))),
    };
    let text_value = match JsFuture::from(text_promise).await {
        Ok(v) => v,
        Err(e) => {
            // Body read failure mid-stream — same network-class issue as the
            // initial fetch, retry it.
            log::debug!("alerts: body read error: {}", err_text(e));
            return Verdict::Retry { after: None };
        }
    };
    let body = match text_value.as_string() {
        Some(s) => s,
        None => return Verdict::Terminal("body not a string".into()),
    };

    Verdict::Ok(FetchOutcome::Updated { body, etag })
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
