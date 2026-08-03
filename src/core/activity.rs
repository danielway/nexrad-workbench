//! The unified acquisition-activity view-model.
//!
//! This is the single honest answer to "what is the app doing right now?".
//! Before it existed, four different structures each tracked a different
//! notion of "in flight" and the UI read whichever one was nearest to hand,
//! so the chip, the timeline, and the dev drawer could all disagree on
//! screen at the same moment.
//!
//! # One number, one meaning
//!
//! Every readout has exactly one authoritative source, and the sources are
//! assigned to **disjoint stages** so a scan is never counted twice:
//!
//! | Source | Authoritative for | Deliberately not used for |
//! |---|---|---|
//! | `AcquisitionState.operations` (Queued/Active) | the headline count, and the Queued + Downloading stages | Processing — an operation completes when the HTTP body lands, well before the data is usable |
//! | [`WorkerLoad`] | the Processing stage | downloads |
//! | [`LedgerSummary::awaiting_timeline`] | the Finishing stage — the ingest→timeline-refresh blackout no other structure covers | Queued/Downloading, whose scans it overlaps |
//! | `NetworkStats::active_count` ([`ActivityInputs::http_in_flight`]) | a dev detail row only | the headline — S3 listing GETs during a pan would read as "4 downloads" and be a lie |
//! | `DownloadProgress` | timeline ghost geometry (not an input here at all) | any stage — one global phase enum cannot describe four parallel downloads |
//!
//! **The headline count is `queued + downloading` and nothing else.**
//! Processing and Finishing change the *word*, never the number, so `↓ 3`
//! always means "3 scans still to fetch" and always matches the number of
//! actionable rows in the sheet.

use std::collections::VecDeque;

use crate::core::acquisition::LedgerSummary;
use crate::core::timeline_view::SCAN_JOIN_TOLERANCE_SECS;
use crate::core::{
    describe_operation, operation_bytes, shows_in_activity_list, AcquisitionOperation,
    NetworkAggregate, NetworkRequest, OperationId, OperationKind, OperationStatus, StreamActivity,
    ThroughputWindow, WorkerLoad,
};

/// How long a non-empty [`WorkerLoad`] is trusted after the last worker
/// outcome reached the main thread.
///
/// A worker that dies mid-job leaves its pending-map entry behind forever,
/// which would pin the Processing stage on for the rest of the session. Past
/// this window we report zero instead: a missing indicator is a smaller lie
/// than a stuck one.
pub(crate) const PROCESSING_STALE_MS: f64 = 60_000.0;

/// Cap on rows materialized for the sheet. The operation ring holds 200; a
/// user scrolling recent downloads does not need all of them, and the strings
/// are built every frame the sheet is open.
const MAX_ROWS: usize = 60;

/// Cap on recent-request rows shown in the Details disclosure.
const MAX_NETWORK_ROWS: usize = 40;

// ───────────────────────────────────────────────────────────────────────────
// Inputs
// ───────────────────────────────────────────────────────────────────────────

/// Everything `build` reads. Assembled once per frame by the shell.
pub(crate) struct ActivityInputs<'a> {
    pub now_ms: f64,
    pub operations: &'a VecDeque<AcquisitionOperation>,
    pub queue_paused: bool,
    pub ledger: LedgerSummary,
    /// Scan starts in the ingest→timeline blackout, used to subtract entries a
    /// live operation already accounts for.
    pub awaiting_scan_starts: &'a [i64],
    pub worker: WorkerLoad,
    pub last_worker_outcome_ms: f64,
    pub gpu_render_in_flight: bool,
    pub throughput: &'a ThroughputWindow,
    /// Raw HTTP requests in flight. Dev detail only — see the module docs.
    pub http_in_flight: u32,
    pub session_requests: u32,
    pub session_bytes: u64,
    pub sw_aggregate: &'a NetworkAggregate,
    pub recent_requests: &'a VecDeque<NetworkRequest>,
    pub cache_size_bytes: u64,
    pub streaming: bool,
    pub stream_activity: StreamActivity,
    /// Whether the activity sheet is open. Rows and network entries are only
    /// materialized when something is actually looking at them.
    pub sheet_open: bool,
    /// Deep diagnostics, present only in dev mode.
    pub dev: Option<ActivityDevInputs>,
}

/// Dev-only inputs, surfaced under the sheet's Details disclosure.
#[derive(Default, Clone, Copy)]
pub(crate) struct ActivityDevInputs {
    pub fps: Option<f64>,
    pub cross_origin_isolated: bool,
    pub avg_fetch_ms: Option<f64>,
    pub avg_processing_ms: Option<f64>,
    pub avg_render_ms: Option<f64>,
    /// Whether a live VCP has been seen, so the forecast diagnostics are
    /// worth offering.
    pub vcp_forecast_available: bool,
    /// Most recent ingest sub-phase timings, as `(label, ms)` pairs in
    /// pipeline order. `None` until an ingest has completed.
    pub ingest_phases: Option<[(&'static str, f64); 6]>,
    /// Most recent render sub-phase timings, as `(label, ms)` pairs in
    /// pipeline order. `None` until a render has completed.
    pub render_phases: Option<[(&'static str, f64); 4]>,
}

// ───────────────────────────────────────────────────────────────────────────
// View-model
// ───────────────────────────────────────────────────────────────────────────

/// What the app is doing, as one mutually exclusive state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityState {
    /// Nothing outstanding. The chip stays visible and says so — an ambient
    /// "up to date" is information, whereas a chip that vanishes is ambiguous
    /// between "idle" and "broken".
    UpToDate,
    /// Archive scans are queued or downloading.
    Downloading { scans: u32 },
    /// Downloads are drained but data is still being decoded or settling.
    Processing,
    /// Live stream is running and no archive work is outstanding.
    Streaming,
    /// The user paused the queue with work still in it.
    Paused { queued: u32 },
}

/// Which glyph the chip shows. Shape, not hue, carries the state — the UI must
/// read in grayscale (PRODUCT §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityGlyph {
    Check,
    ArrowDown,
    Gear,
    Wave,
    Pause,
}

/// The chip's headline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityHeadline {
    /// Scans still to fetch. `None` when there is no count worth showing.
    pub count: Option<u32>,
    pub label: &'static str,
    pub glyph: ActivityGlyph,
    /// Whether motion is appropriate. Motion means "data is moving" and
    /// nothing else; the shell still ANDs this with the reduced-motion check.
    pub animate: bool,
}

/// One stage of the acquisition pipeline, as shown in the sheet's stage strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityStageKind {
    Queued,
    Downloading,
    Processing,
    /// Ingested, waiting for the timeline to observe it.
    Finishing,
}

impl ActivityStageKind {
    #[allow(dead_code)] // Rendered by the activity sheet's stage strip.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            ActivityStageKind::Queued => "Queued",
            ActivityStageKind::Downloading => "Downloading",
            ActivityStageKind::Processing => "Processing",
            ActivityStageKind::Finishing => "Finishing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityStage {
    pub kind: ActivityStageKind,
    pub count: u32,
    pub active: bool,
}

/// Status of one row in the sheet's operation list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RowStatus {
    Queued { position: usize },
    Downloading,
    Completed { duration_ms: f64 },
    Failed { error: String },
    Cancelled,
}

/// One archive download, as the sheet renders it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivityRow {
    pub id: OperationId,
    pub title: String,
    pub status: RowStatus,
    pub bytes: Option<u64>,
    /// Whether `bytes` is the flat per-scan estimate rather than a measurement.
    pub bytes_estimated: bool,
    pub can_cancel: bool,
    pub can_retry: bool,
    pub can_reorder: bool,
}

/// Failures, tracked independently of [`ActivityState`] — a failed scan
/// coexists with active downloads and never suppresses them (PRODUCT §11.2:
/// failures are per-cell, not global).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FailureSummary {
    pub count: u32,
    pub first_error: Option<String>,
    pub retryable: Vec<OperationId>,
}

/// Transfer rate readout. Absent when nothing is moving — the shell renders
/// that as "—", never as `0 B/s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThroughputReadout {
    pub bytes_per_sec: f64,
    pub samples: usize,
}

/// Cumulative session figures shown in the Details disclosure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionTotals {
    pub requests: u32,
    pub failed_requests: u32,
    pub bytes: u64,
    pub cache_bytes: u64,
}

/// One recent network request, projected for display.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NetworkRow {
    pub label: String,
    pub status: u16,
    pub ok: bool,
    pub bytes: u64,
    pub duration_ms: f64,
    pub age_ms: f64,
}

/// Dev-only detail rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ActivityDetailVm {
    pub worker: WorkerLoad,
    pub http_in_flight: u32,
    pub fps: Option<f64>,
    pub cross_origin_isolated: bool,
    pub avg_fetch_ms: Option<f64>,
    pub avg_processing_ms: Option<f64>,
    pub avg_render_ms: Option<f64>,
    pub vcp_forecast_available: bool,
    /// Most recent ingest sub-phase timings, `(label, ms)` in pipeline order.
    pub ingest_phases: Option<[(&'static str, f64); 6]>,
    /// Most recent render sub-phase timings, `(label, ms)` in pipeline order.
    pub render_phases: Option<[(&'static str, f64); 4]>,
    /// Scans the ledger has given up on: the volume genuinely lacks the
    /// requested cut, so no amount of retrying will produce it.
    pub unavailable: u32,
}

/// The complete activity projection, rebuilt once per frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivityVm {
    pub state: ActivityState,
    pub headline: ActivityHeadline,
    pub stages: [ActivityStage; 4],
    pub failed: FailureSummary,
    pub throughput: Option<ThroughputReadout>,
    pub session: SessionTotals,
    /// Empty unless the sheet is open.
    pub rows: Vec<ActivityRow>,
    /// Empty unless the sheet is open.
    pub network: Vec<NetworkRow>,
    pub detail: Option<ActivityDetailVm>,
}

// ───────────────────────────────────────────────────────────────────────────
// Build
// ───────────────────────────────────────────────────────────────────────────

impl ActivityVm {
    pub(crate) fn build(inputs: ActivityInputs<'_>) -> Self {
        let counts = stage_counts(&inputs);
        let failed = failure_summary(inputs.operations);
        let state = derive_state(&counts, &inputs);

        let rows = if inputs.sheet_open {
            build_rows(inputs.operations)
        } else {
            Vec::new()
        };
        let network = if inputs.sheet_open {
            build_network_rows(inputs.recent_requests, inputs.now_ms)
        } else {
            Vec::new()
        };

        Self {
            state,
            headline: headline_for(state, &counts),
            stages: [
                ActivityStage {
                    kind: ActivityStageKind::Queued,
                    count: counts.queued,
                    active: counts.queued > 0,
                },
                ActivityStage {
                    kind: ActivityStageKind::Downloading,
                    count: counts.downloading,
                    active: counts.downloading > 0,
                },
                ActivityStage {
                    kind: ActivityStageKind::Processing,
                    count: counts.processing,
                    active: counts.processing > 0,
                },
                ActivityStage {
                    kind: ActivityStageKind::Finishing,
                    count: counts.settling,
                    active: counts.settling > 0,
                },
            ],
            failed,
            throughput: inputs
                .throughput
                .rate_bytes_per_sec(inputs.now_ms)
                .map(|bytes_per_sec| ThroughputReadout {
                    bytes_per_sec,
                    samples: inputs.throughput.len(),
                }),
            session: SessionTotals {
                // The service-worker aggregate sees *all* traffic (including
                // requests the download channel never issued), so prefer it
                // when the listener is attached and has seen anything.
                requests: if inputs.sw_aggregate.total_requests > 0 {
                    inputs.sw_aggregate.total_requests
                } else {
                    inputs.session_requests
                },
                failed_requests: inputs.sw_aggregate.failed_requests,
                bytes: if inputs.sw_aggregate.total_bytes > 0 {
                    inputs.sw_aggregate.total_bytes
                } else {
                    inputs.session_bytes
                },
                cache_bytes: inputs.cache_size_bytes,
            },
            rows,
            network,
            detail: inputs.dev.map(|dev| ActivityDetailVm {
                worker: inputs.worker,
                http_in_flight: inputs.http_in_flight,
                fps: dev.fps,
                cross_origin_isolated: dev.cross_origin_isolated,
                avg_fetch_ms: dev.avg_fetch_ms,
                avg_processing_ms: dev.avg_processing_ms,
                avg_render_ms: dev.avg_render_ms,
                vcp_forecast_available: dev.vcp_forecast_available,
                ingest_phases: dev.ingest_phases,
                render_phases: dev.render_phases,
                unavailable: inputs.ledger.unavailable,
            }),
        }
    }
}

/// The four disjoint stage counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StageCounts {
    queued: u32,
    downloading: u32,
    processing: u32,
    settling: u32,
}

impl StageCounts {
    /// The headline figure: scans the user is still waiting to *fetch*.
    fn outstanding(&self) -> u32 {
        self.queued + self.downloading
    }
}

fn stage_counts(inputs: &ActivityInputs<'_>) -> StageCounts {
    let mut queued = 0;
    let mut downloading = 0;
    for op in inputs.operations {
        if !shows_in_activity_list(&op.kind) {
            continue;
        }
        match op.status {
            OperationStatus::Queued => queued += 1,
            OperationStatus::Active => downloading += 1,
            _ => {}
        }
    }

    // A worker that never returns would otherwise pin this on forever.
    let stale = inputs.now_ms - inputs.last_worker_outcome_ms > PROCESSING_STALE_MS;
    let processing = if stale {
        0
    } else {
        inputs.worker.total() as u32
    };

    StageCounts {
        queued,
        downloading,
        processing,
        settling: settling_count(inputs),
    }
}

/// Ledger entries in the ingest→timeline blackout, minus any that a live
/// operation already accounts for.
///
/// Without the subtraction a scan that is still `Active` in the queue *and*
/// freshly ingested would appear in both Downloading and Finishing, which is
/// exactly the double-counting this module exists to prevent.
fn settling_count(inputs: &ActivityInputs<'_>) -> u32 {
    let live_scan_starts: Vec<i64> = inputs
        .operations
        .iter()
        .filter(|op| matches!(op.status, OperationStatus::Queued | OperationStatus::Active))
        .filter_map(|op| match &op.kind {
            OperationKind::ArchiveDownload { scan_start, .. } => Some(*scan_start),
            _ => None,
        })
        .collect();

    inputs
        .awaiting_scan_starts
        .iter()
        .filter(|&&awaiting| {
            !live_scan_starts
                .iter()
                .any(|&live| (live - awaiting).abs() <= SCAN_JOIN_TOLERANCE_SECS)
        })
        .count() as u32
}

fn derive_state(counts: &StageCounts, inputs: &ActivityInputs<'_>) -> ActivityState {
    let outstanding = counts.outstanding();
    if inputs.queue_paused && outstanding > 0 {
        return ActivityState::Paused {
            queued: outstanding,
        };
    }
    if outstanding > 0 {
        return ActivityState::Downloading { scans: outstanding };
    }
    if counts.processing > 0 || counts.settling > 0 || inputs.gpu_render_in_flight {
        return ActivityState::Processing;
    }
    if inputs.streaming
        && matches!(
            inputs.stream_activity,
            StreamActivity::Connecting | StreamActivity::Receiving | StreamActivity::Waiting
        )
    {
        return ActivityState::Streaming;
    }
    ActivityState::UpToDate
}

fn headline_for(state: ActivityState, counts: &StageCounts) -> ActivityHeadline {
    match state {
        ActivityState::UpToDate => ActivityHeadline {
            count: None,
            label: "Up to date",
            glyph: ActivityGlyph::Check,
            animate: false,
        },
        ActivityState::Downloading { scans } => ActivityHeadline {
            count: Some(scans),
            label: "Downloading",
            glyph: ActivityGlyph::ArrowDown,
            animate: counts.downloading > 0,
        },
        ActivityState::Processing => ActivityHeadline {
            count: None,
            label: "Processing",
            glyph: ActivityGlyph::Gear,
            animate: true,
        },
        ActivityState::Streaming => ActivityHeadline {
            count: None,
            label: "Receiving",
            glyph: ActivityGlyph::Wave,
            animate: true,
        },
        ActivityState::Paused { queued } => ActivityHeadline {
            count: Some(queued),
            label: "Paused",
            glyph: ActivityGlyph::Pause,
            animate: false,
        },
    }
}

fn failure_summary(operations: &VecDeque<AcquisitionOperation>) -> FailureSummary {
    let mut summary = FailureSummary::default();
    for op in operations {
        if !shows_in_activity_list(&op.kind) {
            continue;
        }
        if let OperationStatus::Failed { error } = &op.status {
            summary.count += 1;
            if summary.first_error.is_none() {
                summary.first_error = Some(error.clone());
            }
            summary.retryable.push(op.id);
        }
    }
    summary
}

fn build_rows(operations: &VecDeque<AcquisitionOperation>) -> Vec<ActivityRow> {
    // Queue position is 1-based over Queued ops in dispatch order, so it must
    // be assigned before the newest-first reversal.
    let mut position = 0usize;
    let mut positions: Vec<Option<usize>> = Vec::with_capacity(operations.len());
    for op in operations {
        if shows_in_activity_list(&op.kind) && op.status == OperationStatus::Queued {
            position += 1;
            positions.push(Some(position));
        } else {
            positions.push(None);
        }
    }

    operations
        .iter()
        .zip(positions)
        .rev()
        .filter(|(op, _)| shows_in_activity_list(&op.kind))
        .take(MAX_ROWS)
        .map(|(op, position)| {
            let status = match &op.status {
                OperationStatus::Queued => RowStatus::Queued {
                    position: position.unwrap_or(0),
                },
                OperationStatus::Active => RowStatus::Downloading,
                OperationStatus::Completed { duration_ms, .. } => RowStatus::Completed {
                    duration_ms: *duration_ms,
                },
                OperationStatus::Failed { error } => RowStatus::Failed {
                    error: error.clone(),
                },
                OperationStatus::Cancelled => RowStatus::Cancelled,
            };
            ActivityRow {
                id: op.id,
                title: describe_operation(&op.kind),
                bytes: operation_bytes(&op.status, &op.kind),
                bytes_estimated: !matches!(op.status, OperationStatus::Completed { .. }),
                can_cancel: matches!(status, RowStatus::Queued { .. }),
                can_retry: matches!(status, RowStatus::Failed { .. }),
                can_reorder: matches!(status, RowStatus::Queued { .. }),
                status,
            }
        })
        .collect()
}

fn build_network_rows(requests: &VecDeque<NetworkRequest>, now_ms: f64) -> Vec<NetworkRow> {
    requests
        .iter()
        .rev()
        .take(MAX_NETWORK_ROWS)
        .map(|req| NetworkRow {
            label: shorten_url(&req.url),
            status: req.status,
            ok: req.ok,
            bytes: req.bytes,
            duration_ms: req.duration_ms,
            age_ms: (now_ms - req.timestamp_ms).max(0.0),
        })
        .collect()
}

/// Reduce a URL to the part a human reads: the last path segment, without a
/// query string. Falls back to the whole URL when there is nothing to trim.
pub(crate) fn shorten_url(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    let trimmed = without_query.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::acquisition::LedgerSummary;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn download(id: OperationId, scan_start: i64, status: OperationStatus) -> AcquisitionOperation {
        AcquisitionOperation {
            id,
            kind: OperationKind::ArchiveDownload {
                site_id: "KDMX".into(),
                file_name: format!("KDMX_{scan_start:06}"),
                scan_start,
                scan_end: scan_start + 300,
            },
            status,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }

    fn chunk(id: OperationId, status: OperationStatus) -> AcquisitionOperation {
        AcquisitionOperation {
            id,
            kind: OperationKind::RealtimeChunk {
                site_id: "KDMX".into(),
                chunk_index: 1,
                is_start: false,
                is_end: false,
                scan_timestamp: 1000,
            },
            status,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }

    /// Fixture owning the borrowed inputs, so tests can build a VM in one line.
    struct Fixture {
        operations: VecDeque<AcquisitionOperation>,
        awaiting: Vec<i64>,
        throughput: ThroughputWindow,
        aggregate: NetworkAggregate,
        requests: VecDeque<NetworkRequest>,
        queue_paused: bool,
        worker: WorkerLoad,
        last_worker_outcome_ms: f64,
        gpu_render_in_flight: bool,
        http_in_flight: u32,
        streaming: bool,
        stream_activity: StreamActivity,
        sheet_open: bool,
        dev: Option<ActivityDevInputs>,
        now_ms: f64,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                operations: VecDeque::new(),
                awaiting: Vec::new(),
                throughput: ThroughputWindow::default(),
                aggregate: NetworkAggregate::default(),
                requests: VecDeque::new(),
                queue_paused: false,
                worker: WorkerLoad::default(),
                // Fresh by default, so the staleness backstop is not in play.
                last_worker_outcome_ms: 1_000.0,
                gpu_render_in_flight: false,
                http_in_flight: 0,
                streaming: false,
                stream_activity: StreamActivity::Off,
                sheet_open: false,
                dev: None,
                now_ms: 1_000.0,
            }
        }

        fn vm(&self) -> ActivityVm {
            ActivityVm::build(ActivityInputs {
                now_ms: self.now_ms,
                operations: &self.operations,
                queue_paused: self.queue_paused,
                ledger: LedgerSummary::default(),
                awaiting_scan_starts: &self.awaiting,
                worker: self.worker,
                last_worker_outcome_ms: self.last_worker_outcome_ms,
                gpu_render_in_flight: self.gpu_render_in_flight,
                throughput: &self.throughput,
                http_in_flight: self.http_in_flight,
                session_requests: 0,
                session_bytes: 0,
                sw_aggregate: &self.aggregate,
                recent_requests: &self.requests,
                cache_size_bytes: 0,
                streaming: self.streaming,
                stream_activity: self.stream_activity,
                sheet_open: self.sheet_open,
                dev: self.dev,
            })
        }
    }

    fn ingesting(n: usize) -> WorkerLoad {
        WorkerLoad {
            ingest: n,
            ..WorkerLoad::default()
        }
    }

    // ── state & precedence ──────────────────────────────────────────────────

    /// An idle app says so rather than showing nothing — a chip that vanishes
    /// is ambiguous between "idle" and "broken".
    #[wasm_bindgen_test]
    fn up_to_date_when_nothing_outstanding() {
        let vm = Fixture::new().vm();
        assert_eq!(vm.state, ActivityState::UpToDate);
        assert_eq!(vm.headline.count, None);
        assert_eq!(vm.headline.label, "Up to date");
        assert_eq!(vm.headline.glyph, ActivityGlyph::Check);
        assert!(!vm.headline.animate);
    }

    /// The headline count is Queued + Active, and nothing else.
    #[wasm_bindgen_test]
    fn downloading_counts_queued_plus_active_only() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.operations
            .push_back(download(2, 2000, OperationStatus::Active));
        f.operations.push_back(download(
            3,
            3000,
            OperationStatus::Completed {
                duration_ms: 10.0,
                bytes: 5,
            },
        ));
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Downloading { scans: 2 });
        assert_eq!(vm.headline.count, Some(2));
        assert_eq!(vm.stages[0].count, 1); // Queued
        assert_eq!(vm.stages[1].count, 1); // Downloading
    }

    /// Once downloads drain but the worker is still chewing, the state becomes
    /// Processing rather than falling straight to "up to date".
    #[wasm_bindgen_test]
    fn processing_state_when_downloads_drained_but_worker_busy() {
        let mut f = Fixture::new();
        f.worker = ingesting(2);
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Processing);
        assert_eq!(vm.headline.label, "Processing");
        assert_eq!(vm.stages[2].count, 2);
        assert!(vm.stages[2].active);
    }

    /// A GPU upload in flight also counts as processing, so the indicator
    /// doesn't blink off between decode and paint.
    #[wasm_bindgen_test]
    fn gpu_render_in_flight_reads_as_processing() {
        let mut f = Fixture::new();
        f.gpu_render_in_flight = true;
        assert_eq!(f.vm().state, ActivityState::Processing);
    }

    /// Pause outranks downloading: the user explicitly stopped the queue, and
    /// showing "Downloading" while nothing moves would be a lie.
    #[wasm_bindgen_test]
    fn paused_outranks_downloading() {
        let mut f = Fixture::new();
        f.queue_paused = true;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Paused { queued: 1 });
        assert_eq!(vm.headline.glyph, ActivityGlyph::Pause);
        assert!(!vm.headline.animate);
    }

    /// Pausing an empty queue is not a state — there is nothing being held.
    #[wasm_bindgen_test]
    fn paused_with_empty_queue_is_up_to_date() {
        let mut f = Fixture::new();
        f.queue_paused = true;
        assert_eq!(f.vm().state, ActivityState::UpToDate);
    }

    /// Archive work outranks the live stream: the user is waiting on the
    /// scans they asked for, not on the background feed.
    #[wasm_bindgen_test]
    fn streaming_state_only_when_no_archive_work() {
        let mut f = Fixture::new();
        f.streaming = true;
        f.stream_activity = StreamActivity::Receiving;
        assert_eq!(f.vm().state, ActivityState::Streaming);

        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        assert_eq!(f.vm().state, ActivityState::Downloading { scans: 1 });
    }

    /// A stalled stream is not "receiving" — it must not animate as if data
    /// were arriving.
    #[wasm_bindgen_test]
    fn stalled_stream_is_not_streaming_state() {
        let mut f = Fixture::new();
        f.streaming = true;
        f.stream_activity = StreamActivity::Stalled;
        assert_eq!(f.vm().state, ActivityState::UpToDate);
    }

    /// Failures never suppress the activity state: a failed scan coexists with
    /// active downloads (PRODUCT §11.2, failures are per-cell not global).
    #[wasm_bindgen_test]
    fn failed_count_coexists_with_downloading() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        f.operations.push_back(download(
            2,
            2000,
            OperationStatus::Failed {
                error: "boom".into(),
            },
        ));
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Downloading { scans: 1 });
        assert_eq!(vm.failed.count, 1);
        assert_eq!(vm.failed.first_error.as_deref(), Some("boom"));
        assert_eq!(vm.failed.retryable, vec![2]);
    }

    /// A worker that died leaves a pending entry behind forever. Past the
    /// staleness window we report zero rather than a permanently stuck
    /// "Processing".
    #[wasm_bindgen_test]
    fn stale_worker_load_does_not_report_processing() {
        let mut f = Fixture::new();
        f.worker = ingesting(3);
        f.last_worker_outcome_ms = 0.0;
        f.now_ms = PROCESSING_STALE_MS + 1.0;
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::UpToDate);
        assert_eq!(vm.stages[2].count, 0);
    }

    /// Just inside the window the load is still trusted.
    #[wasm_bindgen_test]
    fn fresh_worker_load_is_trusted_at_the_boundary() {
        let mut f = Fixture::new();
        f.worker = ingesting(3);
        f.last_worker_outcome_ms = 0.0;
        f.now_ms = PROCESSING_STALE_MS;
        assert_eq!(f.vm().stages[2].count, 3);
    }

    // ── reconciliation ──────────────────────────────────────────────────────

    /// Raw HTTP requests (S3 listings during a pan) must never inflate the
    /// headline — the count means "scans", not "sockets".
    #[wasm_bindgen_test]
    fn chip_count_ignores_http_in_flight() {
        let mut f = Fixture::new();
        f.http_in_flight = 3;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        let vm = f.vm();
        assert_eq!(vm.headline.count, Some(1));
        assert_eq!(vm.state, ActivityState::Downloading { scans: 1 });
    }

    /// The double-count guard: a scan that is still an in-flight operation and
    /// also freshly ingested belongs to Downloading only, never to both.
    #[wasm_bindgen_test]
    fn settling_excludes_scans_a_live_op_already_covers() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        // Within the join tolerance of the live op's scan_start.
        f.awaiting = vec![1000 + SCAN_JOIN_TOLERANCE_SECS - 1];
        assert_eq!(f.vm().stages[3].count, 0);

        // A different scan, well outside the tolerance, does count.
        f.awaiting = vec![9_000];
        assert_eq!(f.vm().stages[3].count, 1);
    }

    /// With downloads drained, a settling scan keeps the app in Processing so
    /// the chip can't claim "up to date" mid-ingest.
    #[wasm_bindgen_test]
    fn settling_alone_reads_as_processing() {
        let mut f = Fixture::new();
        f.awaiting = vec![1000];
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Processing);
        assert_eq!(vm.stages[3].count, 1);
    }

    /// Realtime chunk bookkeeping is plumbing; it must not appear as downloads
    /// the user is waiting on.
    #[wasm_bindgen_test]
    fn realtime_chunk_ops_do_not_inflate_the_download_count() {
        let mut f = Fixture::new();
        f.operations.push_back(chunk(1, OperationStatus::Active));
        f.operations.push_back(chunk(2, OperationStatus::Queued));
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::UpToDate);
        assert_eq!(vm.stages[0].count, 0);
        assert_eq!(vm.stages[1].count, 0);
    }

    /// Cancelled operations are finished work — they belong in no stage and in
    /// no failure count.
    #[wasm_bindgen_test]
    fn cancelled_ops_are_excluded_from_every_count() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Cancelled));
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::UpToDate);
        assert_eq!(vm.stages.iter().map(|s| s.count).sum::<u32>(), 0);
        assert_eq!(vm.failed.count, 0);
    }

    /// The headline number is fetch-work only: adding processing and settling
    /// changes the label but must leave the count alone.
    #[wasm_bindgen_test]
    fn processing_and_settling_never_change_the_headline_count() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        let before = f.vm().headline.count;

        f.worker = ingesting(4);
        f.awaiting = vec![50_000];
        let after = f.vm();
        assert_eq!(after.headline.count, before);
        assert_eq!(after.headline.count, Some(1));
        // ...but the stages do reflect the extra work.
        assert_eq!(after.stages[2].count, 4);
        assert_eq!(after.stages[3].count, 1);
    }

    // ── rows ────────────────────────────────────────────────────────────────

    /// Rows are newest-first so the most recent download is at the top.
    #[wasm_bindgen_test]
    fn rows_are_newest_first() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.operations
            .push_back(download(2, 2000, OperationStatus::Queued));
        let vm = f.vm();
        assert_eq!(vm.rows.len(), 2);
        assert_eq!(vm.rows[0].id, 2);
        assert_eq!(vm.rows[1].id, 1);
    }

    /// Nothing is materialized while the sheet is closed — the chip needs no
    /// rows, and building 200 strings per frame for a hidden modal is waste.
    #[wasm_bindgen_test]
    fn rows_are_empty_when_the_sheet_is_closed() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.requests.push_back(NetworkRequest {
            url: "https://example.com/a/b.gz".into(),
            status: 200,
            bytes: 10,
            duration_ms: 5.0,
            ok: true,
            timestamp_ms: 900.0,
            operation_id: None,
        });
        let vm = f.vm();
        assert!(vm.rows.is_empty());
        assert!(vm.network.is_empty());
        // The counts still work — they don't depend on the rows.
        assert_eq!(vm.headline.count, Some(1));
    }

    /// Queue position is 1-based in dispatch order, even though rows render
    /// newest-first.
    #[wasm_bindgen_test]
    fn queued_row_carries_its_queue_position() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.operations
            .push_back(download(2, 2000, OperationStatus::Active));
        f.operations
            .push_back(download(3, 3000, OperationStatus::Queued));
        let vm = f.vm();
        // Newest first: id 3 (2nd in queue), id 2 (active), id 1 (1st in queue).
        assert_eq!(vm.rows[0].status, RowStatus::Queued { position: 2 });
        assert_eq!(vm.rows[1].status, RowStatus::Downloading);
        assert_eq!(vm.rows[2].status, RowStatus::Queued { position: 1 });
    }

    /// A failed row offers retry and carries its error; a queued row offers
    /// cancel and reorder instead.
    #[wasm_bindgen_test]
    fn row_affordances_match_status() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.operations.push_back(download(
            2,
            2000,
            OperationStatus::Failed {
                error: "timeout".into(),
            },
        ));
        let vm = f.vm();
        let failed = &vm.rows[0];
        assert!(failed.can_retry);
        assert!(!failed.can_cancel);
        assert_eq!(
            failed.status,
            RowStatus::Failed {
                error: "timeout".into()
            }
        );

        let queued = &vm.rows[1];
        assert!(queued.can_cancel);
        assert!(queued.can_reorder);
        assert!(!queued.can_retry);
    }

    /// Sizes are real for completed downloads and flagged as estimates
    /// otherwise, so the UI can render the "~" prefix honestly.
    #[wasm_bindgen_test]
    fn row_bytes_estimated_for_queued_real_for_completed() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.operations
            .push_back(download(1, 1000, OperationStatus::Queued));
        f.operations.push_back(download(
            2,
            2000,
            OperationStatus::Completed {
                duration_ms: 1.0,
                bytes: 1234,
            },
        ));
        let vm = f.vm();
        assert_eq!(vm.rows[0].bytes, Some(1234));
        assert!(!vm.rows[0].bytes_estimated);
        assert_eq!(vm.rows[1].bytes, Some(crate::core::AVG_SCAN_BYTES));
        assert!(vm.rows[1].bytes_estimated);
    }

    /// Only archive downloads produce rows; chunk plumbing stays out.
    #[wasm_bindgen_test]
    fn only_archive_downloads_produce_rows() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.operations.push_back(chunk(1, OperationStatus::Active));
        f.operations
            .push_back(download(2, 2000, OperationStatus::Active));
        let vm = f.vm();
        assert_eq!(vm.rows.len(), 1);
        assert_eq!(vm.rows[0].id, 2);
    }

    /// The row list is capped so a full 200-entry ring can't blow up the frame.
    #[wasm_bindgen_test]
    fn rows_are_capped() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        for i in 0..(MAX_ROWS + 25) {
            f.operations.push_back(download(
                i as u64,
                i as i64 * 1000,
                OperationStatus::Cancelled,
            ));
        }
        assert_eq!(f.vm().rows.len(), MAX_ROWS);
    }

    // ── throughput, totals, network rows ────────────────────────────────────

    /// No samples means no readout — the shell shows "—", never "0 B/s".
    #[wasm_bindgen_test]
    fn throughput_absent_when_idle() {
        assert_eq!(Fixture::new().vm().throughput, None);
    }

    /// With samples, the readout carries both the rate and the sample count.
    #[wasm_bindgen_test]
    fn throughput_present_when_samples_exist() {
        let mut f = Fixture::new();
        f.throughput.push(
            crate::core::ThroughputSample {
                at_ms: 0.0,
                bytes: 2_000,
            },
            0.0,
        );
        f.now_ms = 1_000.0;
        let readout = f.vm().throughput.expect("rate");
        assert_eq!(readout.bytes_per_sec, 2_000.0);
        assert_eq!(readout.samples, 1);
    }

    /// The service-worker aggregate wins when it has seen traffic, since it
    /// observes requests the download channel never issued.
    #[wasm_bindgen_test]
    fn session_totals_prefer_the_service_worker_aggregate() {
        let mut f = Fixture::new();
        f.aggregate = NetworkAggregate {
            total_requests: 42,
            failed_requests: 2,
            total_bytes: 9_000,
        };
        let vm = f.vm();
        assert_eq!(vm.session.requests, 42);
        assert_eq!(vm.session.failed_requests, 2);
        assert_eq!(vm.session.bytes, 9_000);
    }

    /// Without a service worker, the channel counters are used instead.
    #[wasm_bindgen_test]
    fn session_totals_fall_back_to_channel_counters() {
        let f = Fixture::new();
        let vm = ActivityVm::build(ActivityInputs {
            session_requests: 7,
            session_bytes: 700,
            ..ActivityInputs {
                now_ms: f.now_ms,
                operations: &f.operations,
                queue_paused: false,
                ledger: LedgerSummary::default(),
                awaiting_scan_starts: &f.awaiting,
                worker: f.worker,
                last_worker_outcome_ms: f.last_worker_outcome_ms,
                gpu_render_in_flight: false,
                throughput: &f.throughput,
                http_in_flight: 0,
                session_requests: 0,
                session_bytes: 0,
                sw_aggregate: &f.aggregate,
                recent_requests: &f.requests,
                cache_size_bytes: 0,
                streaming: false,
                stream_activity: StreamActivity::Off,
                sheet_open: false,
                dev: None,
            }
        });
        assert_eq!(vm.session.requests, 7);
        assert_eq!(vm.session.bytes, 700);
    }

    /// Network rows are newest-first and carry a non-negative age.
    #[wasm_bindgen_test]
    fn network_rows_are_newest_first_with_age() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.now_ms = 5_000.0;
        for (i, ts) in [1_000.0, 2_000.0].into_iter().enumerate() {
            f.requests.push_back(NetworkRequest {
                url: format!("https://s3.example.com/bucket/file{i}.gz?x=1"),
                status: 200,
                bytes: 10,
                duration_ms: 5.0,
                ok: true,
                timestamp_ms: ts,
                operation_id: None,
            });
        }
        let vm = f.vm();
        assert_eq!(vm.network.len(), 2);
        assert_eq!(vm.network[0].label, "file1.gz");
        assert_eq!(vm.network[0].age_ms, 3_000.0);
        assert_eq!(vm.network[1].label, "file0.gz");
    }

    /// A clock skew that would make a request "arrive in the future" clamps to
    /// zero rather than rendering a negative age.
    #[wasm_bindgen_test]
    fn network_row_age_never_goes_negative() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.now_ms = 0.0;
        f.requests.push_back(NetworkRequest {
            url: "https://example.com/x".into(),
            status: 200,
            bytes: 1,
            duration_ms: 1.0,
            ok: true,
            timestamp_ms: 5_000.0,
            operation_id: None,
        });
        assert_eq!(f.vm().network[0].age_ms, 0.0);
    }

    // ── shorten_url ─────────────────────────────────────────────────────────

    /// The last path segment is what identifies a request to a human.
    #[wasm_bindgen_test]
    fn shorten_url_keeps_the_last_segment() {
        assert_eq!(
            shorten_url("https://s3.amazonaws.com/bucket/KDMX20240501_120000_V06"),
            "KDMX20240501_120000_V06"
        );
    }

    /// Query strings are noise in a request list.
    #[wasm_bindgen_test]
    fn shorten_url_drops_the_query_string() {
        assert_eq!(
            shorten_url("https://x.com/a/b.gz?list-type=2&max=5"),
            "b.gz"
        );
    }

    /// A trailing slash shouldn't yield an empty label.
    #[wasm_bindgen_test]
    fn shorten_url_ignores_a_trailing_slash() {
        assert_eq!(shorten_url("https://x.com/bucket/"), "bucket");
    }

    /// Anything unparseable degrades to the original string rather than "".
    #[wasm_bindgen_test]
    fn shorten_url_falls_back_to_the_whole_url() {
        assert_eq!(shorten_url("plain"), "plain");
        assert_eq!(shorten_url(""), "");
    }

    // ── dev detail ──────────────────────────────────────────────────────────

    /// Deep diagnostics are absent entirely outside dev mode, so the shell has
    /// nothing to conditionally hide.
    #[wasm_bindgen_test]
    fn detail_absent_without_dev_inputs() {
        assert!(Fixture::new().vm().detail.is_none());
    }

    /// In dev mode the detail carries the raw worker load and HTTP figure that
    /// the user-facing stages deliberately don't expose.
    #[wasm_bindgen_test]
    fn detail_carries_worker_and_http_figures() {
        let mut f = Fixture::new();
        f.http_in_flight = 4;
        f.worker = ingesting(2);
        f.dev = Some(ActivityDevInputs {
            fps: Some(59.5),
            cross_origin_isolated: true,
            ..ActivityDevInputs::default()
        });
        let detail = f.vm().detail.expect("detail");
        assert_eq!(detail.http_in_flight, 4);
        assert_eq!(detail.worker.ingest, 2);
        assert_eq!(detail.fps, Some(59.5));
        assert!(detail.cross_origin_isolated);
    }
}
