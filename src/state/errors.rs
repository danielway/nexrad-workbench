//! App-side error aggregation.
//!
//! Before this module, errors were surfaced inconsistently — some as
//! `status_message` flips, some as the dedicated `worker_init_error`
//! banner, some only logged. The design calls for a single collector
//! that all reporters push to, so the UI can surface a coherent
//! recent-errors view instead of multiple ad-hoc indicators.
//!
//! This is the seed: a small ring buffer on [`AppState`]
//! ([`crate::state::AppState::errors`]) plus the [`AppError`] taxonomy.
//! Reporters migrate to it incrementally — the worker-error handler is
//! the first.

use std::collections::VecDeque;

use crate::nexrad::WorkerErrorKind;

/// Cap on retained recent-error entries. Old entries fall off when
/// pushing past this size; the buffer is a quick "what just went
/// wrong?" surface, not a long-term log.
const MAX_RETAINED: usize = 50;

/// An error worth surfacing to the user or recording for diagnostics.
///
/// Variants identify the originating subsystem so the UI can show an
/// appropriate icon / link without parsing the message string.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `Other` is reserved for future reporters not yet migrated.
pub enum AppError {
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
    pub fn source_label(&self) -> &'static str {
        match self {
            AppError::Worker { .. } => "worker",
            AppError::Download { .. } => "download",
            AppError::Alerts { .. } => "alerts",
            AppError::Mping { .. } => "mping",
            AppError::Other { .. } => "other",
        }
    }

    /// The user-facing message.
    pub fn message(&self) -> &str {
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
pub struct TimestampedError {
    pub error: AppError,
    /// Wall-clock time the error was pushed (milliseconds since epoch).
    pub timestamp_ms: f64,
}

/// Recent-errors ring buffer. Lives on [`crate::state::AppState`] and
/// receives pushes from reporters across the codebase.
#[derive(Default)]
pub struct ErrorContext {
    recent: VecDeque<TimestampedError>,
}

impl ErrorContext {
    /// Push a fresh error. Older entries fall off when the ring exceeds
    /// [`MAX_RETAINED`].
    pub fn push(&mut self, error: AppError) {
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
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &TimestampedError> {
        self.recent.iter()
    }

    /// Most recent error, if any.
    #[allow(dead_code)] // Available for callers; current UI iterates instead.
    pub fn most_recent(&self) -> Option<&TimestampedError> {
        self.recent.back()
    }

    /// Number of retained entries.
    pub fn len(&self) -> usize {
        self.recent.len()
    }

    /// Whether the ring is currently empty.
    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }

    /// Clear the ring.
    pub fn clear(&mut self) {
        self.recent.clear();
    }
}
