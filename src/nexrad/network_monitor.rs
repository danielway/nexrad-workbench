//! Service worker network monitoring.
//!
//! Listens for `network-metric` messages from the service worker and
//! accumulates per-request telemetry into a ring buffer (for the UI request
//! log) and aggregate session statistics.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Maximum number of recent requests to keep in the ring buffer.
const MAX_RECENT_REQUESTS: usize = 100;

/// A single completed network request reported by the service worker.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct NetworkRequest {
    /// Request URL (truncated for display).
    pub url: String,
    /// HTTP status code (0 if the request failed before a response).
    pub status: u16,
    /// Response body size in bytes (from Content-Length).
    pub bytes: u64,
    /// Duration of the request in milliseconds.
    pub duration_ms: f64,
    /// Whether the response was successful (2xx).
    pub ok: bool,
    /// Timestamp when this metric was received (ms since epoch).
    pub timestamp_ms: f64,
    /// Error message, if the request failed.
    pub error: Option<String>,
}

/// Aggregate network statistics for the session.
#[derive(Clone, Debug, Default)]
pub struct NetworkAggregate {
    /// Total number of requests intercepted.
    pub total_requests: u32,
    /// Number of failed requests (non-ok or network error).
    pub failed_requests: u32,
    /// Total bytes transferred.
    pub total_bytes: u64,
}

/// Listens for service worker messages and accumulates network metrics.
///
/// Holds a JS closure that prevents garbage collection of the event listener.
pub struct NetworkMonitor {
    recent_requests: Rc<RefCell<VecDeque<NetworkRequest>>>,
    aggregate: Rc<RefCell<NetworkAggregate>>,
    _listener: Closure<dyn FnMut(web_sys::MessageEvent)>,
}

impl NetworkMonitor {
    /// Create a new monitor and attach a message listener to the service worker
    /// container. Returns `None` if service workers are not available.
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let navigator = window.navigator();
        let sw_container = navigator.service_worker();

        let recent_requests: Rc<RefCell<VecDeque<NetworkRequest>>> =
            Rc::new(RefCell::new(VecDeque::with_capacity(MAX_RECENT_REQUESTS)));
        let aggregate: Rc<RefCell<NetworkAggregate>> =
            Rc::new(RefCell::new(NetworkAggregate::default()));

        let recent_clone = recent_requests.clone();
        let agg_clone = aggregate.clone();

        let listener =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                let data = event.data();

                // Only process our network-metric messages
                let msg_type = js_sys::Reflect::get(&data, &"type".into())
                    .ok()
                    .and_then(|v| v.as_string());
                if msg_type.as_deref() != Some("network-metric") {
                    return;
                }

                let url = js_sys::Reflect::get(&data, &"url".into())
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let status = js_sys::Reflect::get(&data, &"status".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as u16;
                let bytes = js_sys::Reflect::get(&data, &"bytes".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as u64;
                let duration = js_sys::Reflect::get(&data, &"duration".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let ok = js_sys::Reflect::get(&data, &"ok".into())
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let error = js_sys::Reflect::get(&data, &"error".into())
                    .ok()
                    .and_then(|v| v.as_string());

                let req = NetworkRequest {
                    url,
                    status,
                    bytes,
                    duration_ms: duration,
                    ok,
                    timestamp_ms: js_sys::Date::now(),
                    error,
                };

                // Update aggregate
                {
                    let mut agg = agg_clone.borrow_mut();
                    agg.total_requests += 1;
                    if !ok {
                        agg.failed_requests += 1;
                    }
                    agg.total_bytes += bytes;
                }

                // Push into ring buffer
                {
                    let mut recent = recent_clone.borrow_mut();
                    if recent.len() >= MAX_RECENT_REQUESTS {
                        recent.pop_front();
                    }
                    recent.push_back(req);
                }
            });

        sw_container
            .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
            .ok()?;

        log::info!("NetworkMonitor: listening for service worker metrics");

        Some(Self {
            recent_requests,
            aggregate,
            _listener: listener,
        })
    }

    /// Get a snapshot of the current aggregate statistics.
    pub fn aggregate(&self) -> NetworkAggregate {
        self.aggregate.borrow().clone()
    }

    /// Drain all recent requests accumulated since the last drain.
    /// Returns them in chronological order.
    pub fn drain_recent(&self) -> Vec<NetworkRequest> {
        let recent = self.recent_requests.borrow();
        recent.iter().cloned().collect()
    }

}

/// Check whether the current browsing context is cross-origin isolated
/// (i.e., `self.crossOriginIsolated` is true), which means `SharedArrayBuffer`
/// is available.
pub fn is_cross_origin_isolated() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &"crossOriginIsolated".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
