//! Service worker network monitoring.
//!
//! Listens for `network-metric` messages from the service worker and
//! accumulates per-request telemetry into a pending queue (for the UI
//! request log) and aggregate session statistics.

use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Cap on the pending buffer. The main loop drains each frame so this
/// should never be reached in practice; it only bounds memory if the
/// UI stalls for long enough that thousands of metrics accumulate.
const MAX_PENDING_REQUESTS: usize = 500;

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
    /// Correlated acquisition operation ID (populated by URL matching in main loop).
    pub operation_id: Option<crate::state::OperationId>,
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
    pending: Rc<RefCell<Vec<NetworkRequest>>>,
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

        let pending: Rc<RefCell<Vec<NetworkRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let aggregate: Rc<RefCell<NetworkAggregate>> =
            Rc::new(RefCell::new(NetworkAggregate::default()));

        let pending_clone = pending.clone();
        let agg_clone = aggregate.clone();

        let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
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
                    operation_id: None,
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

                // Queue for the main loop to drain.
                {
                    let mut pending = pending_clone.borrow_mut();
                    if pending.len() >= MAX_PENDING_REQUESTS {
                        pending.remove(0);
                    }
                    pending.push(req);
                }
            },
        );

        sw_container
            .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
            .ok()?;

        log::debug!("NetworkMonitor: listening for service worker metrics");

        Some(Self {
            pending,
            aggregate,
            _listener: listener,
        })
    }

    /// Get a snapshot of the current aggregate statistics.
    pub fn aggregate(&self) -> NetworkAggregate {
        self.aggregate.borrow().clone()
    }

    /// Take all requests accumulated since the last call.
    ///
    /// Returns an empty `Vec` (no allocation beyond the swap-in) when no
    /// new metrics have arrived, so the common idle case pays only a
    /// borrow-and-check.
    pub fn take_pending(&self) -> Vec<NetworkRequest> {
        let mut pending = self.pending.borrow_mut();
        if pending.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *pending)
        }
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
