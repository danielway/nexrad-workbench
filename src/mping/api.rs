//! mPING reports API fetch logic.
//!
//! Uses the browser Fetch API via web-sys. The endpoint requires an
//! `Authorization: Token <key>` header — note the literal word "Token",
//! not "Bearer". The official API does not document a CORS policy, so a
//! preflight failure is possible and is surfaced verbatim to the user
//! through `MpingEvent::Error`.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::channel::{MpingChannel, MpingEvent};
use super::parse::parse_response;
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};

/// Base URL for the mPING v2 reports endpoint.
const REPORTS_URL: &str = "https://mping.ou.edu/mping/api/v2/reports";

/// Maximum results to request per page. The API uses Django REST
/// Framework pagination; for a 30-min × 300 km window this is comfortably
/// above any realistic count.
const PAGE_SIZE: u32 = 200;

/// Parameters for a single fetch request.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchParams {
    /// Center longitude (degrees) for the radius filter.
    pub center_lon: f64,
    /// Center latitude (degrees) for the radius filter.
    pub center_lat: f64,
    /// Radius (meters) for the spatial filter.
    pub radius_m: u32,
    /// Inclusive lower bound on observation time, milliseconds since epoch.
    pub min_obtime_ms: i64,
    /// Inclusive upper bound on observation time, milliseconds since epoch.
    pub max_obtime_ms: i64,
}

/// Spawn a background fetch. Results are pushed into `channel` when done.
pub fn spawn_fetch(
    ctx: eframe::egui::Context,
    channel: MpingChannel,
    api_key: String,
    params: FetchParams,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let event = match fetch_inner(&api_key, &params).await {
            Ok(body) => match parse_response(&body) {
                Ok(parsed) => MpingEvent::Updated {
                    reports: parsed.reports,
                    total_count: parsed.total_count,
                },
                Err(e) => MpingEvent::Error(format!("parse failed: {}", e)),
            },
            Err(e) => MpingEvent::Error(e),
        };
        channel.push(event);
        ctx.request_repaint();
    });
}

async fn fetch_inner(api_key: &str, params: &FetchParams) -> Result<String, String> {
    let url = build_url(params);
    let auth_header = format!("Token {}", api_key);

    with_retry(&DEFAULT_POLICY, "mping", |_attempt| {
        let url = url.clone();
        let auth_header = auth_header.clone();
        async move { fetch_attempt(&url, &auth_header).await }
    })
    .await
}

fn build_url(params: &FetchParams) -> String {
    let min_iso = chrono::DateTime::from_timestamp_millis(params.min_obtime_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let max_iso = chrono::DateTime::from_timestamp_millis(params.max_obtime_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    format!(
        "{}?obtime_gte={}&obtime_lte={}&dist={}&point={},{}&page_size={}",
        REPORTS_URL,
        urlencode(&min_iso),
        urlencode(&max_iso),
        params.radius_m,
        params.center_lon,
        params.center_lat,
        PAGE_SIZE,
    )
}

/// Minimal percent-encoder for the values we put in query strings (ISO
/// timestamps contain `:` and `+`, both of which must be escaped). Avoids
/// pulling in a new dependency.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Run a single fetch attempt and classify its outcome.
async fn fetch_attempt(url: &str, auth_header: &str) -> Verdict<String> {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return Verdict::Terminal("no window".into()),
    };

    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);

    let headers = match web_sys::Headers::new() {
        Ok(h) => h,
        Err(_) => return Verdict::Terminal("failed to allocate headers".into()),
    };
    let _ = headers.set("Accept", "application/json");
    let _ = headers.set("Authorization", auth_header);
    init.set_headers(&JsValue::from(headers));

    let request = match web_sys::Request::new_with_str_and_init(url, &init) {
        Ok(r) => r,
        Err(e) => return Verdict::Terminal(format!("request init failed: {:?}", e)),
    };

    // Network-layer error (DNS, CORS preflight failure, …) — surface
    // immediately as terminal so the user sees a clear, actionable
    // message rather than five seconds of silent retries.
    let resp_value = match JsFuture::from(window.fetch_with_request(&request)).await {
        Ok(v) => v,
        Err(e) => {
            return Verdict::Terminal(format!(
                "network/CORS error: {} \u{2014} mPING may not allow direct browser access from this origin",
                err_text(e)
            ));
        }
    };

    let resp: web_sys::Response = match resp_value.dyn_into() {
        Ok(r) => r,
        Err(_) => return Verdict::Terminal("invalid response object".into()),
    };

    let status = resp.status();
    // Auth failures are terminal — retrying with the same key won't help.
    if status == 401 || status == 403 {
        return Verdict::Terminal(format!("HTTP {} \u{2014} check your API key", status));
    }
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        return Verdict::Retry { after: None };
    }
    if !resp.ok() {
        return Verdict::Terminal(format!("HTTP {}", status));
    }

    let text_promise = match resp.text() {
        Ok(p) => p,
        Err(e) => return Verdict::Terminal(format!("failed to read body: {}", err_text(e))),
    };
    let text_value = match JsFuture::from(text_promise).await {
        Ok(v) => v,
        Err(e) => {
            log::debug!("mping: body read error: {}", err_text(e));
            return Verdict::Retry { after: None };
        }
    };
    match text_value.as_string() {
        Some(s) => Verdict::Ok(s),
        None => Verdict::Terminal("body not a string".into()),
    }
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
