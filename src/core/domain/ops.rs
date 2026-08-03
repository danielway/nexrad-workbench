//! Acquisition operation vocabulary — the pure types every layer speaks.
//!
//! An *operation* is one user-recognisable unit of acquisition work: a single
//! archive scan download, an S3 listing, or one realtime chunk. It carries an
//! identity, a status, and (for downloads) the affordances the user acts on —
//! cancel, reorder, retry.
//!
//! These types live in the core so the pure activity view-model
//! ([`crate::core::activity`]) can read them: `core` may not import `state`,
//! and the container that holds them (`state::AcquisitionState`) legitimately
//! belongs to `state` because it calls the clock. Vocabulary down here,
//! container up there.

/// Unique identifier for an acquisition operation.
pub(crate) type OperationId = u64;

/// Flat per-scan size estimate used wherever a real byte count is unavailable.
///
/// The S3 listing in this client's `nexrad-data` version exposes only file
/// names, not sizes, so queued and in-flight downloads have no true size until
/// the body lands. Any readout derived from this must be marked as an estimate
/// (`~5 MB`), never presented as measured.
pub(crate) const AVG_SCAN_BYTES: u64 = 5 * 1024 * 1024;

/// Which phase of the download pipeline a scan is in.
///
/// This describes the *global* pipeline position tracked by
/// `state::DownloadProgress` for timeline ghosts. It is deliberately not a
/// per-operation field: up to four downloads run in parallel, so a single
/// phase enum cannot honestly describe all of them at once. Per-stage counts
/// for the activity surface come from disjoint sources instead — see
/// [`crate::core::activity`].
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub(crate) enum DownloadPhase {
    #[default]
    Idle,
    /// Fetching from AWS S3.
    Downloading,
    /// Worker is splitting, decompressing, decoding, and storing in IDB.
    Ingesting,
    /// Worker is decoding and rendering the sweep.
    Decoding,
    /// Complete.
    Done,
}

/// The kind of acquisition operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OperationKind {
    /// Archive listing fetch (S3 LIST).
    #[allow(dead_code)] // Vocabulary; listing fetches don't create operations today.
    ArchiveListing {
        site_id: String,
        date: chrono::NaiveDate,
    },
    /// Archive scan download (S3 GET for a volume file).
    ArchiveDownload {
        site_id: String,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
    },
    /// Realtime chunk acquisition.
    RealtimeChunk {
        site_id: String,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        /// Volume start timestamp (Unix seconds) shared by all chunks in the same scan.
        scan_timestamp: i64,
    },
}

/// Status of an acquisition operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OperationStatus {
    /// Waiting in queue.
    Queued,
    /// Currently downloading.
    Active,
    /// Successfully completed.
    Completed { duration_ms: f64, bytes: u64 },
    /// Failed with an error message.
    Failed { error: String },
    /// Cancelled by user or selection change.
    Cancelled,
}

/// A single acquisition operation.
#[derive(Clone, Debug)]
pub(crate) struct AcquisitionOperation {
    pub id: OperationId,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub started_at_ms: Option<f64>,
    pub completed_at_ms: Option<f64>,
}

/// A short human-readable description of an operation.
pub(crate) fn describe_operation(kind: &OperationKind) -> String {
    match kind {
        OperationKind::ArchiveListing { site_id, date } => {
            format!("List {} {}", site_id, date)
        }
        OperationKind::ArchiveDownload {
            site_id, file_name, ..
        } => {
            // Extract the HHMMSS time portion following the first `_`.
            // Every slice is taken with `get` (char-boundary-safe): the
            // outer `get(..i+7)` only guarantees a 6-byte string, but a
            // multi-byte char inside it would make the inner byte offsets
            // (0/2/4) land mid-codepoint, so byte-indexing `&t[0..2]` would
            // panic the egui frame on a malformed (non-ASCII) name. Any
            // `None` falls back to the raw file name.
            //
            // NB: this anchors on the first `_` (variable position),
            // whereas `archive_index::parse_timestamp_from_name` reads a
            // fixed position-13 window — different rules, so not factored
            // into a shared helper.
            let time_part = file_name
                .find('_')
                .and_then(|i| file_name.get(i + 1..i + 7))
                .and_then(|t| {
                    Some(format!(
                        "{}:{}:{}",
                        t.get(0..2)?,
                        t.get(2..4)?,
                        t.get(4..6)?
                    ))
                })
                .unwrap_or_else(|| file_name.clone());
            format!("{} {}", site_id, time_part)
        }
        OperationKind::RealtimeChunk {
            site_id,
            chunk_index,
            scan_timestamp,
            ..
        } => {
            // Format scan timestamp as HH:MM:SS UTC for display
            let dt = chrono::DateTime::from_timestamp(*scan_timestamp, 0);
            if let Some(dt) = dt {
                format!(
                    "{} live {} chunk #{}",
                    site_id,
                    dt.format("%H:%M:%S"),
                    chunk_index
                )
            } else {
                format!("{} chunk #{}", site_id, chunk_index)
            }
        }
    }
}

/// Whether an operation belongs in the user-facing activity list.
///
/// Archive downloads are the user's "downloads"; listing and realtime-chunk
/// bookkeeping is plumbing that would only add noise to the queue readout.
pub(crate) fn shows_in_activity_list(kind: &OperationKind) -> bool {
    matches!(kind, OperationKind::ArchiveDownload { .. })
}

/// Byte size to display for an operation.
///
/// Completed operations carry their real transferred byte count. Active and
/// queued downloads have no real size (see [`AVG_SCAN_BYTES`]) so they fall
/// back to the flat per-scan estimate. Returns `None` for kinds with no
/// meaningful size (listings, realtime chunks) and for cancelled/failed ops.
pub(crate) fn operation_bytes(status: &OperationStatus, kind: &OperationKind) -> Option<u64> {
    match status {
        OperationStatus::Completed { bytes, .. } => Some(*bytes),
        OperationStatus::Queued | OperationStatus::Active => match kind {
            OperationKind::ArchiveDownload { .. } => Some(AVG_SCAN_BYTES),
            _ => None,
        },
        OperationStatus::Failed { .. } | OperationStatus::Cancelled => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn download_kind() -> OperationKind {
        OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: "KDMX_1000".to_string(),
            scan_start: 1000,
            scan_end: 1300,
        }
    }

    fn listing_kind() -> OperationKind {
        OperationKind::ArchiveListing {
            site_id: "KDMX".into(),
            date: chrono::NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),
        }
    }

    // ── describe_operation ──────────────────────────────────────────────────

    /// The HHMMSS extraction must never panic on a multi-byte filename. A name
    /// whose 6-byte window after the first `_` straddles a multi-byte char
    /// makes the inner 0/2/4 byte offsets land mid-codepoint; `get` returns
    /// `None` there and we fall back to the raw name instead of panicking.
    #[wasm_bindgen_test]
    fn describe_operation_does_not_panic_on_multibyte_name() {
        let kind = OperationKind::ArchiveDownload {
            site_id: "KDMX".into(),
            // "é" is 2 bytes, so the 6-byte window is not 6 chars.
            file_name: "KDMX_é2345678".into(),
            scan_start: 0,
            scan_end: 0,
        };
        let desc = describe_operation(&kind);
        assert!(desc.starts_with("KDMX "), "got {desc}");
    }

    /// A well-formed ASCII name still renders as HH:MM:SS.
    #[wasm_bindgen_test]
    fn describe_operation_formats_ascii_time() {
        let kind = OperationKind::ArchiveDownload {
            site_id: "KDMX".into(),
            file_name: "KDMX20240501_123456_V06".into(),
            scan_start: 0,
            scan_end: 0,
        };
        assert_eq!(describe_operation(&kind), "KDMX 12:34:56");
    }

    /// Listings render "List SITE DATE".
    #[wasm_bindgen_test]
    fn describe_operation_listing() {
        assert_eq!(describe_operation(&listing_kind()), "List KDMX 2024-05-01");
    }

    /// Realtime chunks format the scan timestamp; an out-of-range timestamp
    /// falls back to the site + chunk index without a time.
    #[wasm_bindgen_test]
    fn describe_operation_realtime_valid_and_invalid_ts() {
        let valid = OperationKind::RealtimeChunk {
            site_id: "KDMX".into(),
            chunk_index: 3,
            is_start: false,
            is_end: false,
            scan_timestamp: 0,
        };
        assert_eq!(describe_operation(&valid), "KDMX live 00:00:00 chunk #3");

        let invalid = OperationKind::RealtimeChunk {
            site_id: "KDMX".into(),
            chunk_index: 3,
            is_start: false,
            is_end: false,
            scan_timestamp: i64::MAX,
        };
        assert_eq!(describe_operation(&invalid), "KDMX chunk #3");
    }

    // ── list membership + sizes ─────────────────────────────────────────────

    /// Only archive downloads reach the user-facing list; listing and realtime
    /// plumbing stays out of it.
    #[wasm_bindgen_test]
    fn only_archive_downloads_show_in_the_activity_list() {
        assert!(shows_in_activity_list(&download_kind()));
        assert!(!shows_in_activity_list(&listing_kind()));
        assert!(!shows_in_activity_list(&OperationKind::RealtimeChunk {
            site_id: "KDMX".into(),
            chunk_index: 1,
            is_start: true,
            is_end: false,
            scan_timestamp: 1000,
        }));
    }

    /// Completed ops report their real transferred bytes (no estimate).
    #[wasm_bindgen_test]
    fn completed_op_uses_real_bytes() {
        let s = OperationStatus::Completed {
            duration_ms: 1000.0,
            bytes: 7_654_321,
        };
        assert_eq!(operation_bytes(&s, &download_kind()), Some(7_654_321));
    }

    /// Queued / active archive downloads fall back to the flat per-scan
    /// estimate (real S3 sizes aren't available to the client).
    #[wasm_bindgen_test]
    fn queued_and_active_archive_downloads_estimate_avg_scan_bytes() {
        for s in [OperationStatus::Queued, OperationStatus::Active] {
            assert_eq!(operation_bytes(&s, &download_kind()), Some(AVG_SCAN_BYTES));
        }
    }

    /// Failed / cancelled ops, and non-download kinds, carry no size readout.
    #[wasm_bindgen_test]
    fn no_size_for_failed_cancelled_or_non_download() {
        assert_eq!(
            operation_bytes(
                &OperationStatus::Failed { error: "x".into() },
                &download_kind()
            ),
            None
        );
        assert_eq!(
            operation_bytes(&OperationStatus::Cancelled, &download_kind()),
            None
        );
        assert_eq!(
            operation_bytes(&OperationStatus::Queued, &listing_kind()),
            None
        );
    }
}
