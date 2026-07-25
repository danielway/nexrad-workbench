//! Network telemetry records reported by the service worker.
//!
//! Pure data types only — the listener that accumulates them lives in
//! [`crate::subsystem::network_monitor`].

use crate::core::OperationId;

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
    pub operation_id: Option<OperationId>,
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
