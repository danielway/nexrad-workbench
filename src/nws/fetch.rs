//! Async polling for NWS weather alerts via api.weather.gov.

use super::alerts::{parse_alerts_geojson, NwsAlert};
use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Result of an NWS alert fetch.
pub enum NwsAlertResult {
    /// Successfully parsed alerts.
    Success(Vec<NwsAlert>),
    /// Fetch or parse error.
    Error(String),
}

/// Async poller for NWS weather alerts using a channel-based pattern.
pub struct NwsAlertPoller {
    sender: Sender<NwsAlertResult>,
    receiver: Receiver<NwsAlertResult>,
    /// Wall-clock time (seconds) of last poll start.
    last_poll_time: f64,
    /// Whether a fetch is currently in flight.
    polling_active: Rc<RefCell<bool>>,
    /// Minimum seconds between poll attempts.
    poll_interval_secs: f64,
}

impl NwsAlertPoller {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver,
            last_poll_time: 0.0,
            polling_active: Rc::new(RefCell::new(false)),
            poll_interval_secs: 60.0,
        }
    }

    /// Start a poll if the interval has elapsed and no fetch is in flight.
    pub fn poll_if_needed(&mut self, ctx: &egui::Context, lat: f64, lon: f64) {
        let now = js_sys::Date::now() / 1000.0;

        if *self.polling_active.borrow() {
            return;
        }

        if now - self.last_poll_time < self.poll_interval_secs {
            return;
        }

        self.last_poll_time = now;
        *self.polling_active.borrow_mut() = true;

        let sender = self.sender.clone();
        let active = self.polling_active.clone();
        let ctx = ctx.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let result = fetch_alerts(lat, lon).await;
            *active.borrow_mut() = false;
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    /// Non-blocking check for a completed fetch result.
    pub fn try_recv(&mut self) -> Option<NwsAlertResult> {
        self.receiver.try_recv().ok()
    }
}

/// Fetch active alerts from the NWS API for a given point.
async fn fetch_alerts(lat: f64, lon: f64) -> NwsAlertResult {
    use wasm_bindgen::JsCast;

    let url = format!(
        "https://api.weather.gov/alerts/active?point={:.4},{:.4}&status=actual",
        lat, lon
    );

    log::info!("Fetching NWS alerts: {}", url);

    let window = match web_sys::window() {
        Some(w) => w,
        None => return NwsAlertResult::Error("No browser window".to_string()),
    };

    // Use fetch_with_str for a simple GET request (no custom headers needed from browser)
    let resp_value = match wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(&url)).await {
        Ok(v) => v,
        Err(_) => return NwsAlertResult::Error("Network error fetching NWS alerts".to_string()),
    };

    let resp: web_sys::Response = match resp_value.dyn_into() {
        Ok(r) => r,
        Err(_) => return NwsAlertResult::Error("Invalid response from NWS API".to_string()),
    };

    if !resp.ok() {
        return NwsAlertResult::Error(format!("NWS API returned status {}", resp.status()));
    }

    // Read response body as text
    let text_promise = match resp.text() {
        Ok(p) => p,
        Err(_) => return NwsAlertResult::Error("Failed to read NWS response body".to_string()),
    };

    let text_value = match wasm_bindgen_futures::JsFuture::from(text_promise).await {
        Ok(v) => v,
        Err(_) => return NwsAlertResult::Error("Failed to read NWS response text".to_string()),
    };

    let text = match text_value.as_string() {
        Some(s) => s,
        None => return NwsAlertResult::Error("NWS response body is not text".to_string()),
    };

    match parse_alerts_geojson(&text) {
        Ok(alerts) => {
            log::info!("Fetched {} NWS alerts", alerts.len());
            NwsAlertResult::Success(alerts)
        }
        Err(e) => NwsAlertResult::Error(format!("Failed to parse NWS alerts: {}", e)),
    }
}
