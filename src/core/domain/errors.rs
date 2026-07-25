//! App-side error aggregation.
//!
//! Before this module, errors were surfaced inconsistently — some as
//! `status_message` flips, some as the dedicated `worker_init_error`
//! banner, some only logged. The design calls for a single collector
//! that all reporters push to, so the UI can surface a coherent
//! recent-errors view instead of multiple ad-hoc indicators.
//!
//! This is the seed: a small ring buffer on [`AppState`]
//! (`crate::state::AppState::errors`) plus the [`AppError`] taxonomy.
//! Reporters migrate to it incrementally — the worker-error handler is
//! the first.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Cap on retained recent-error entries. Old entries fall off when
/// pushing past this size; the buffer is a quick "what just went
/// wrong?" surface, not a long-term log.
const MAX_RETAINED: usize = 50;

/// Classification of a worker error.
///
/// Carried across the worker boundary as a snake_case JSON tag so callers
/// can dispatch on the error category (prompt the user, retry silently,
/// offer to free space) instead of doing brittle string checks on the
/// `message` field.
///
/// The wire format is owned by `worker.js`'s `classifyError` and pinned by
/// `tests::worker_error_kind_deserializes_known_strings` below.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerErrorKind {
    /// Browser storage quota exceeded — user must free space.
    QuotaExceeded,
    /// IDB read/write failure (transient or DB corruption).
    IdbFailure,
    /// Requested sweep/scan not found in the cache.
    NotFound,
    /// Decoded data malformed or version-mismatched.
    InvalidData,
    /// Worker WASM initialization failed.
    InitFailed,
    /// Unclassified failure (default when no kind is supplied).
    #[serde(other)]
    Unknown,
}

/// An error worth surfacing to the user or recording for diagnostics.
///
/// Variants identify the originating subsystem so the UI can show an
/// appropriate icon / link without parsing the message string.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `Other` is reserved for future reporters not yet migrated.
pub(crate) enum AppError {
    /// Failure from the decode worker.
    Worker {
        kind: WorkerErrorKind,
        message: String,
        /// Scan timestamp (Unix seconds) the failing request was for,
        /// when the receive path could correlate it with a pending
        /// context. Lets UI link the error back to a timeline scan.
        scan_timestamp_secs: Option<f64>,
    },
    /// Failure from an archive S3 download.
    Download {
        message: String,
        /// `scan_start` (Unix seconds, archive resolution) of the
        /// failing scan when known.
        scan_start_secs: Option<i64>,
    },
    /// Failure from the NWS alerts polling.
    Alerts { message: String },
    /// Failure from the mPING storm-report fetch.
    Mping { message: String },
    /// Catch-all for anything that doesn't fit a more specific variant.
    Other { message: String },
}

impl AppError {
    /// Short human-readable label for the originating subsystem.
    pub(crate) fn source_label(&self) -> &'static str {
        match self {
            AppError::Worker { .. } => "worker",
            AppError::Download { .. } => "download",
            AppError::Alerts { .. } => "alerts",
            AppError::Mping { .. } => "mping",
            AppError::Other { .. } => "other",
        }
    }

    /// The user-facing message.
    pub(crate) fn message(&self) -> &str {
        match self {
            AppError::Worker { message, .. }
            | AppError::Download { message, .. }
            | AppError::Alerts { message }
            | AppError::Mping { message }
            | AppError::Other { message } => message,
        }
    }
}

/// One entry in the error ring buffer.
#[derive(Debug, Clone)]
pub(crate) struct TimestampedError {
    pub error: AppError,
    /// Wall-clock time the error was pushed (milliseconds since epoch).
    pub timestamp_ms: f64,
}

/// Recent-errors ring buffer. Lives on [`crate::state::AppState`] and
/// receives pushes from reporters across the codebase.
#[derive(Default)]
pub(crate) struct ErrorContext {
    recent: VecDeque<TimestampedError>,
}

impl ErrorContext {
    /// Push a fresh error. Older entries fall off when the ring exceeds
    /// [`MAX_RETAINED`].
    pub(crate) fn push(&mut self, error: AppError) {
        let entry = TimestampedError {
            error,
            timestamp_ms: js_sys::Date::now(),
        };
        if self.recent.len() >= MAX_RETAINED {
            self.recent.pop_front();
        }
        self.recent.push_back(entry);
    }

    /// All retained entries, oldest first. The returned iterator is
    /// double-ended so callers can `.rev()` for newest-first display.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &TimestampedError> {
        self.recent.iter()
    }

    /// Most recent error, if any.
    #[allow(dead_code)] // Available for callers; current UI iterates instead.
    pub(crate) fn most_recent(&self) -> Option<&TimestampedError> {
        self.recent.back()
    }

    /// Number of retained entries.
    pub(crate) fn len(&self) -> usize {
        self.recent.len()
    }

    /// Whether the ring is currently empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }

    /// Clear the ring.
    pub(crate) fn clear(&mut self) {
        self.recent.clear();
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn other(msg: &str) -> AppError {
        AppError::Other {
            message: msg.to_string(),
        }
    }

    #[wasm_bindgen_test]
    fn source_label_identifies_each_subsystem() {
        assert_eq!(
            AppError::Worker {
                kind: WorkerErrorKind::IdbFailure,
                message: "x".into(),
                scan_timestamp_secs: None,
            }
            .source_label(),
            "worker"
        );
        assert_eq!(
            AppError::Download {
                message: "x".into(),
                scan_start_secs: None,
            }
            .source_label(),
            "download"
        );
        assert_eq!(
            AppError::Alerts {
                message: "x".into()
            }
            .source_label(),
            "alerts"
        );
        assert_eq!(
            AppError::Mping {
                message: "x".into()
            }
            .source_label(),
            "mping"
        );
        assert_eq!(other("x").source_label(), "other");
    }

    #[wasm_bindgen_test]
    fn message_returns_each_variants_text() {
        assert_eq!(
            AppError::Worker {
                kind: WorkerErrorKind::NotFound,
                message: "w".into(),
                scan_timestamp_secs: Some(1.0),
            }
            .message(),
            "w"
        );
        assert_eq!(
            AppError::Download {
                message: "d".into(),
                scan_start_secs: Some(5),
            }
            .message(),
            "d"
        );
        assert_eq!(
            AppError::Alerts {
                message: "a".into()
            }
            .message(),
            "a"
        );
        assert_eq!(
            AppError::Mping {
                message: "m".into()
            }
            .message(),
            "m"
        );
        assert_eq!(other("o").message(), "o");
    }

    #[wasm_bindgen_test]
    fn fresh_context_is_empty() {
        let ctx = ErrorContext::default();
        assert_eq!(ctx.len(), 0);
        assert!(ctx.is_empty());
        assert!(ctx.most_recent().is_none());
    }

    #[wasm_bindgen_test]
    fn push_records_in_oldest_first_order() {
        let mut ctx = ErrorContext::default();
        ctx.push(other("first"));
        ctx.push(other("second"));
        assert_eq!(ctx.len(), 2);
        assert!(!ctx.is_empty());
        let msgs: Vec<&str> = ctx.iter().map(|e| e.error.message()).collect();
        assert_eq!(msgs, vec!["first", "second"]);
        assert_eq!(ctx.most_recent().unwrap().error.message(), "second");
    }

    #[wasm_bindgen_test]
    fn ring_buffer_evicts_oldest_past_the_cap() {
        let mut ctx = ErrorContext::default();
        for i in 0..55 {
            ctx.push(other(&format!("e{i}")));
        }
        // Capped at MAX_RETAINED (50); the first 5 fell off.
        assert_eq!(ctx.len(), 50);
        assert_eq!(ctx.iter().next().unwrap().error.message(), "e5");
        assert_eq!(ctx.most_recent().unwrap().error.message(), "e54");
        // Newest-first view via the double-ended iterator.
        assert_eq!(ctx.iter().next_back().unwrap().error.message(), "e54");
    }

    #[wasm_bindgen_test]
    fn clear_empties_the_ring() {
        let mut ctx = ErrorContext::default();
        ctx.push(other("x"));
        ctx.clear();
        assert!(ctx.is_empty());
    }

    #[wasm_bindgen_test]
    fn worker_error_kind_deserializes_known_strings() {
        // Pin the wire format that `worker.js` produces via classifyError.
        // Adding a new variant here REQUIRES a parallel addition in
        // worker.js (so the matching `err.name` branch sends the right
        // tag).
        let cases = [
            ("quota_exceeded", WorkerErrorKind::QuotaExceeded),
            ("idb_failure", WorkerErrorKind::IdbFailure),
            ("not_found", WorkerErrorKind::NotFound),
            ("invalid_data", WorkerErrorKind::InvalidData),
            ("init_failed", WorkerErrorKind::InitFailed),
            ("unknown", WorkerErrorKind::Unknown),
        ];
        for (s, expected) in cases {
            let v = serde_wasm_bindgen::to_value(s).unwrap();
            let parsed: WorkerErrorKind = serde_wasm_bindgen::from_value(v).unwrap();
            assert_eq!(parsed, expected, "round-trip failed for {:?}", s);
        }
    }

    #[wasm_bindgen_test]
    fn worker_error_kind_unknown_tag_falls_back_to_unknown() {
        // serde `#[serde(other)]` on `Unknown` ensures forward-compat:
        // a future kind worker.js learns to emit won't fail deserialization
        // on older clients — it just degrades to `Unknown`.
        let v = serde_wasm_bindgen::to_value("some_future_kind").unwrap();
        let parsed: WorkerErrorKind = serde_wasm_bindgen::from_value(v).unwrap();
        assert_eq!(parsed, WorkerErrorKind::Unknown);
    }
}
