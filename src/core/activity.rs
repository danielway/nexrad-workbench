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
use crate::core::session::Stream;
use crate::core::timeline_view::SCAN_JOIN_TOLERANCE_SECS;
use crate::core::{
    describe_operation, operation_bytes, shows_in_activity_list, AcquisitionOperation,
    ErrorContext, NetworkAggregate, NetworkRequest, OperationId, OperationKind, OperationStatus,
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
    /// Every site's scans in the ingest→timeline blackout, used to subtract
    /// entries a live operation for the same site already accounts for.
    pub awaiting: &'a [AwaitingScan],
    pub worker: WorkerLoad,
    pub last_worker_outcome_ms: f64,
    pub gpu_render_in_flight: bool,
    pub throughput: &'a ThroughputWindow,
    /// Raw HTTP requests in flight. Dev detail only — see the module docs.
    pub http_in_flight: u32,
    pub session_requests: u32,
    pub session_bytes: u64,
    pub sw_aggregate: &'a NetworkAggregate,
    /// Smoothed frame rate, for the always-on readout ([`Readout`]).
    pub fps: Option<f64>,
    pub recent_requests: &'a VecDeque<NetworkRequest>,
    pub cache_size_bytes: u64,
    /// Every site's stream, in list order. The chip projects the healthiest
    /// one's word ([`headline_stream`]) when no archive work is outstanding
    /// and, with more than one site, sums them ([`SiteSum`]).
    pub sites: &'a [SiteStream],
    /// Index into `sites` of the focus, whose section the sheet lists first.
    pub focus: usize,
    /// Whether the activity sheet is open. Rows and network entries are only
    /// materialized when something is actually looking at them.
    pub sheet_open: bool,
    /// Deep diagnostics, present only in dev mode.
    pub dev: Option<ActivityDevInputs>,
    /// The app's recent errors, and how many had been pushed when the sheet
    /// last showed them ([`errors_acked`]).
    pub errors: &'a ErrorContext,
    pub errors_acked: u64,
}

/// Dev-only inputs, surfaced under the sheet's Details disclosure.
#[derive(Default, Clone)]
pub(crate) struct ActivityDevInputs {
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
    /// Live chunk latency: `(avg fetch ms, p95 fetch ms, avg end-to-end ms)`.
    /// `None` until the live stream has produced samples.
    pub chunk_latency: Option<(f64, f64, Option<f64>)>,
    /// The shown storm cell result: counts, cut and worker timing. `None`
    /// while no cells are shown.
    pub storm_cells: Option<crate::core::storm_cells::StormCellStats>,
    /// Every site's volume texture, in list order.
    pub volume_textures: Vec<crate::core::volume_plan::VolumeTexture>,
}

/// A scan in one site's ingest→timeline blackout (its ledger's
/// `awaiting_scan_starts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AwaitingScan {
    pub site: String,
    pub scan_start: i64,
}

/// One site's stream, as the chip and the sheet read it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SiteStream {
    pub id: String,
    pub stream: Stream,
    /// Chunks received in the site's volume in progress.
    pub chunks_received: u32,
    /// Seconds since the newest radial the site's stream received; `None`
    /// before the first one or while not streaming.
    pub last_chunk_age_secs: Option<f64>,
    /// Seconds until a failed stream starts again, while retrying.
    pub retry_in_secs: Option<f64>,
}

/// The stream the chip speaks for: the site's own with one site; with more,
/// the healthiest of them, so one quiet site never hides another receiving.
pub(crate) fn headline_stream(sites: &[SiteStream]) -> Stream {
    const ORDER: [Stream; 7] = [
        Stream::Receiving,
        Stream::Connecting,
        Stream::Waiting,
        Stream::Stalled,
        Stream::Retrying,
        Stream::Failed,
        Stream::Off,
    ];
    ORDER
        .into_iter()
        .find(|s| sites.iter().any(|site| site.stream == *s))
        .unwrap_or(Stream::Off)
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
    /// Live stream is connecting, receiving or waiting, and no archive work is
    /// outstanding.
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
    /// How many sites the chip speaks for, and how many of their streams
    /// stalled.
    pub sites: SiteSum,
}

/// The chip's sum over the sites.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SiteSum {
    pub sites: u32,
    pub stalled: u32,
}

impl SiteSum {
    pub(crate) fn of(sites: &[SiteStream]) -> Self {
        Self {
            sites: sites.len() as u32,
            stalled: sites.iter().filter(|s| s.stream == Stream::Stalled).count() as u32,
        }
    }
}

impl ActivityHeadline {
    /// The chip's words. With one site, the label alone; with more, the
    /// sum: `Receiving · 3 sites`, `Receiving 3 sites · 1 stalled`.
    pub(crate) fn words(&self) -> String {
        let SiteSum { sites, stalled } = self.sites;
        match (sites, stalled) {
            (0 | 1, _) => self.label.to_string(),
            (n, 0) => format!("{} \u{00b7} {n} sites", self.label),
            (n, k) => format!("{} {n} sites \u{00b7} {k} stalled", self.label),
        }
    }
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

/// The app's errors other than downloads (which [`FailureSummary`] owns):
/// decode worker, alerts, mPING. Each also flashes as a toast; this is what
/// stays, so an error the phone's toast came and went on is still on the
/// chip until the sheet has shown it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ErrorSummary {
    /// Errors not yet shown in the sheet.
    pub unseen: u32,
    /// Newest first, at most [`SHEET_ERRORS`]. Empty unless the sheet is
    /// open.
    pub recent: Vec<String>,
}

/// How many recent errors the sheet lists.
pub(crate) const SHEET_ERRORS: usize = 10;

/// Summarize the errors for the chip and the sheet. `acked` is the pushed
/// count when the sheet last showed them. With `name_sites` (more than one
/// site on the map) an error that knows its site leads with it
/// ([`sheet_error_text`]).
pub(crate) fn error_summary(
    errors: &ErrorContext,
    acked: u64,
    sheet_open: bool,
    name_sites: bool,
) -> ErrorSummary {
    let fresh = (errors.pushed_count().saturating_sub(acked) as usize).min(errors.len());
    let unseen = errors
        .iter()
        .skip(errors.len() - fresh)
        .filter(|e| crate::core::toast::error_text(&e.error).is_some())
        .count() as u32;
    let recent = if sheet_open {
        errors
            .iter()
            .rev()
            .filter_map(|e| sheet_error_text(&e.error, name_sites))
            .take(SHEET_ERRORS)
            .collect()
    } else {
        Vec::new()
    };
    ErrorSummary { unseen, recent }
}

/// An error's line in the sheet: the toast's words, led by the error's site
/// (`KMLB · Decode worker: …`) when `name_sites` and the error knows it.
pub(crate) fn sheet_error_text(error: &crate::core::AppError, name_sites: bool) -> Option<String> {
    let text = crate::core::toast::error_text(error)?;
    match error {
        crate::core::AppError::Worker {
            site: Some(site), ..
        } if name_sites => Some(format!("{site} \u{00B7} {text}")),
        _ => Some(text),
    }
}

/// The acknowledged count after this frame: an open sheet has shown every
/// error there is.
pub(crate) fn errors_acked(errors: &ErrorContext, acked: u64, sheet_open: bool) -> u64 {
    if sheet_open {
        errors.pushed_count()
    } else {
        acked
    }
}

/// Transfer rate readout. Absent when nothing is moving — the shell renders
/// that as "—", never as `0 B/s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThroughputReadout {
    pub bytes_per_sec: f64,
    pub samples: usize,
}

/// Cumulative session figures shown in the Details disclosure. The raw
/// request counts are developer rows ([`ActivityDetailVm`]), as PRODUCT.md
/// asks (one-instrument §11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionTotals {
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
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivityDetailVm {
    /// HTTP requests this session (the service worker's count when it has
    /// seen traffic, else the download channel's).
    pub requests: u32,
    pub failed_requests: u32,
    pub worker: WorkerLoad,
    pub http_in_flight: u32,
    pub avg_fetch_ms: Option<f64>,
    pub avg_processing_ms: Option<f64>,
    pub avg_render_ms: Option<f64>,
    pub vcp_forecast_available: bool,
    /// Most recent ingest sub-phase timings, `(label, ms)` in pipeline order.
    pub ingest_phases: Option<[(&'static str, f64); 6]>,
    /// Most recent render sub-phase timings, `(label, ms)` in pipeline order.
    pub render_phases: Option<[(&'static str, f64); 4]>,
    /// Live chunk latency: `(avg fetch ms, p95 fetch ms, avg end-to-end ms)`.
    pub chunk_latency: Option<(f64, f64, Option<f64>)>,
    /// Storm cell detection readout; its
    /// [`label`](crate::core::storm_cells::StormCellStats::label) is the row.
    pub storm_cells: Option<crate::core::storm_cells::StormCellStats>,
    /// Scans the ledger has given up on: the volume genuinely lacks the
    /// requested cut, so no amount of retrying will produce it.
    pub unavailable: u32,
    /// `(label, value)` per site holding a volume texture
    /// ([`crate::core::volume_plan::volume_texture_rows`]).
    pub volume_rows: Vec<(String, String)>,
}

/// The small always-on readout beside the activity chip: how smoothly the app
/// is drawing and how many network requests it has made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Readout {
    /// Frames per second, rounded. `None` before the first frame is timed.
    pub fps: Option<u32>,
    /// Network requests this session ([`session_requests`]).
    pub requests: u32,
}

impl Readout {
    /// The words drawn, e.g. `60 fps · 128 req`. The frame rate is left out
    /// until a frame has been timed.
    pub(crate) fn text(&self) -> String {
        match self.fps {
            Some(fps) => format!("{fps} fps · {} req", self.requests),
            None => format!("{} req", self.requests),
        }
    }
}

/// Network requests this session: the service worker's count when it has
/// seen traffic, since it sees every fetch the page makes (S3 listings and
/// volumes, the activity index, mosaic images), else the download channel's.
fn session_requests(inputs: &ActivityInputs<'_>) -> u32 {
    if inputs.sw_aggregate.total_requests > 0 {
        inputs.sw_aggregate.total_requests
    } else {
        inputs.session_requests
    }
}

/// One site's section in the sheet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SiteSection {
    pub id: String,
    /// The focus site, listed first.
    pub focus: bool,
    /// `Receiving · 12 chunks · last 4 s ago`, or `Not streaming`.
    pub stream_line: String,
    /// Whether the stream is past its cadence or failed (drawn as a
    /// warning).
    pub unhealthy: bool,
    /// This site's failed downloads.
    pub failed: u32,
    /// The first failed download's error.
    pub first_error: Option<String>,
}

/// The sheet's per-site sections: focus first, then the rest in list order.
/// Empty with one site, so the one-site sheet is unchanged.
pub(crate) fn site_sections(
    sites: &[SiteStream],
    focus: usize,
    operations: &VecDeque<AcquisitionOperation>,
) -> Vec<SiteSection> {
    if sites.len() < 2 {
        return Vec::new();
    }
    let order = std::iter::once(focus).chain((0..sites.len()).filter(|&i| i != focus));
    order
        .filter_map(|i| sites.get(i).map(|s| (i, s)))
        .map(|(i, s)| {
            let failures: Vec<&str> = operations
                .iter()
                .filter_map(|op| match (&op.status, &op.kind) {
                    (
                        OperationStatus::Failed { error },
                        OperationKind::ArchiveDownload { site_id, .. },
                    ) if site_id.eq_ignore_ascii_case(&s.id) => Some(error.as_str()),
                    _ => None,
                })
                .collect();
            SiteSection {
                id: s.id.clone(),
                focus: i == focus,
                stream_line: site_stream_line(s),
                unhealthy: matches!(
                    s.stream,
                    Stream::Stalled | Stream::Retrying | Stream::Failed
                ),
                failed: failures.len() as u32,
                first_error: failures.first().map(|e| e.to_string()),
            }
        })
        .collect()
}

/// A site's stream in the sheet: its state, chunk count and last chunk age.
fn site_stream_line(s: &SiteStream) -> String {
    let word = match s.stream {
        Stream::Off => return "Not streaming".to_string(),
        Stream::Failed => return "Stream failed".to_string(),
        Stream::Retrying => {
            return match s.retry_in_secs {
                Some(secs) if secs >= 1.0 => format!(
                    "Stream failed \u{00b7} retrying in {}",
                    crate::core::format_lag(secs.ceil())
                ),
                _ => "Stream failed \u{00b7} retrying".to_string(),
            }
        }
        Stream::Connecting => "Connecting",
        Stream::Receiving => "Receiving",
        Stream::Waiting => "Waiting",
        Stream::Stalled => "Stalled",
    };
    let mut line = format!(
        "{word} \u{00b7} {} chunk{}",
        s.chunks_received,
        if s.chunks_received == 1 { "" } else { "s" }
    );
    if let Some(age) = s.last_chunk_age_secs {
        line.push_str(&format!(
            " \u{00b7} last {} ago",
            crate::core::format_lag(age.max(0.0).round())
        ));
    }
    line
}

/// The complete activity projection, rebuilt once per frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivityVm {
    pub state: ActivityState,
    pub headline: ActivityHeadline,
    pub stages: [ActivityStage; 4],
    pub failed: FailureSummary,
    pub errors: ErrorSummary,
    pub throughput: Option<ThroughputReadout>,
    pub session: SessionTotals,
    pub readout: Readout,
    /// Empty unless the sheet is open.
    pub rows: Vec<ActivityRow>,
    /// Empty unless the sheet is open.
    pub network: Vec<NetworkRow>,
    /// One section per site; empty unless the sheet is open and more than
    /// one site is listed ([`site_sections`]).
    pub sites: Vec<SiteSection>,
    pub detail: Option<ActivityDetailVm>,
}

// ───────────────────────────────────────────────────────────────────────────
// Build
// ───────────────────────────────────────────────────────────────────────────

impl ActivityVm {
    /// The chip's hover sentence. With more than one site it names how many
    /// the chip speaks for and how many of their streams stalled.
    pub(crate) fn hover(&self) -> String {
        let SiteSum { sites, stalled } = self.headline.sites;
        if sites < 2 {
            return match self.state {
                ActivityState::UpToDate => "Everything you're viewing is downloaded",
                ActivityState::Downloading { .. } => "Downloading scans — click for details",
                ActivityState::Processing => "Decoding downloaded data — click for details",
                ActivityState::Streaming => "Live stream running — click for details",
                ActivityState::Paused { .. } => "Downloads paused — click to resume",
            }
            .to_string();
        }
        let stalled = match stalled {
            0 => String::new(),
            1 => ", 1 stream stalled".to_string(),
            k => format!(", {k} streams stalled"),
        };
        match self.state {
            ActivityState::UpToDate => {
                format!("Everything you're viewing on {sites} sites is downloaded{stalled}")
            }
            ActivityState::Downloading { .. } => {
                format!("Downloading scans for {sites} sites{stalled} — click for details")
            }
            ActivityState::Processing => {
                format!("Decoding downloaded data for {sites} sites{stalled} — click for details")
            }
            ActivityState::Streaming => {
                format!("Live streams running on {sites} sites{stalled} — click for details")
            }
            ActivityState::Paused { .. } => {
                format!("Downloads paused for {sites} sites{stalled} — click to resume")
            }
        }
    }

    /// The chip's ⚠ tick: failed downloads plus errors the sheet has not
    /// shown, with its hover. `None` when there is nothing to flag.
    pub(crate) fn warning(&self) -> Option<(u32, String)> {
        let (failed, errors) = (self.failed.count, self.errors.unseen);
        if failed + errors == 0 {
            return None;
        }
        let mut parts = Vec::new();
        if failed > 0 {
            parts.push(match self.failed.first_error.as_deref() {
                Some(err) => format!("Some downloads failed ({err})"),
                None => "Some downloads failed".to_string(),
            });
        }
        if errors > 0 {
            parts.push(format!(
                "{errors} new error{}",
                if errors == 1 { "" } else { "s" }
            ));
        }
        Some((
            failed + errors,
            format!("{} — click for details", parts.join("; ")),
        ))
    }

    /// [`Self::build`], with the chip's word settled through `settle` so it
    /// does not flicker ([`HeadlineSettle`]). The stage counts stay raw: the
    /// sheet shows what is happening this frame.
    pub(crate) fn build_settled(inputs: ActivityInputs<'_>, settle: &mut HeadlineSettle) -> Self {
        let now_ms = inputs.now_ms;
        let stream = headline_stream(inputs.sites);
        let sum = SiteSum::of(inputs.sites);
        let counts = stage_counts(&inputs);
        let mut vm = Self::build(inputs);
        let quiet = if stream.headline_word().is_some() {
            ActivityState::Streaming
        } else {
            ActivityState::UpToDate
        };
        let shown = settle.step(vm.state, quiet, now_ms);
        if shown != vm.state {
            vm.state = shown;
            vm.headline = headline_for(shown, &counts, stream, sum);
        }
        vm
    }

    pub(crate) fn build(inputs: ActivityInputs<'_>) -> Self {
        let counts = stage_counts(&inputs);
        let failed = failure_summary(inputs.operations);
        let stream = headline_stream(inputs.sites);
        let state = derive_state(&counts, &inputs, stream);

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

        let sites = if inputs.sheet_open {
            site_sections(inputs.sites, inputs.focus, inputs.operations)
        } else {
            Vec::new()
        };

        Self {
            state,
            headline: headline_for(state, &counts, stream, SiteSum::of(inputs.sites)),
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
            errors: error_summary(
                inputs.errors,
                inputs.errors_acked,
                inputs.sheet_open,
                inputs.sites.len() > 1,
            ),
            throughput: inputs
                .throughput
                .rate_bytes_per_sec(inputs.now_ms)
                .map(|bytes_per_sec| ThroughputReadout {
                    bytes_per_sec,
                    samples: inputs.throughput.len(),
                }),
            session: SessionTotals {
                bytes: if inputs.sw_aggregate.total_bytes > 0 {
                    inputs.sw_aggregate.total_bytes
                } else {
                    inputs.session_bytes
                },
                cache_bytes: inputs.cache_size_bytes,
            },
            readout: Readout {
                fps: inputs
                    .fps
                    .filter(|fps| fps.is_finite())
                    .map(|fps| fps.round().max(0.0) as u32),
                requests: session_requests(&inputs),
            },
            rows,
            network,
            sites,
            detail: inputs.dev.as_ref().map(|dev| ActivityDetailVm {
                requests: session_requests(&inputs),
                failed_requests: inputs.sw_aggregate.failed_requests,
                worker: inputs.worker,
                http_in_flight: inputs.http_in_flight,
                avg_fetch_ms: dev.avg_fetch_ms,
                avg_processing_ms: dev.avg_processing_ms,
                avg_render_ms: dev.avg_render_ms,
                vcp_forecast_available: dev.vcp_forecast_available,
                ingest_phases: dev.ingest_phases,
                render_phases: dev.render_phases,
                chunk_latency: dev.chunk_latency,
                storm_cells: dev.storm_cells,
                unavailable: inputs.ledger.unavailable,
                volume_rows: crate::core::volume_plan::volume_texture_rows(&dev.volume_textures),
            }),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Settling the headline
// ───────────────────────────────────────────────────────────────────────────

/// How long the pipeline must stay busy before an idle chip says
/// "Processing". Most decodes and GPU uploads finish well inside this, so they
/// never reach the chip at all.
pub(crate) const PROCESSING_ENTER_MS: f64 = 750.0;

/// Once "Processing" is on the chip it stays at least this long.
pub(crate) const PROCESSING_HOLD_MS: f64 = 1_500.0;

/// A pause in the work shorter than this does not count as going idle: it
/// neither restarts the [`PROCESSING_ENTER_MS`] wait nor lets a shown
/// "Processing" drop. Back-to-back jobs leave gaps of a frame or two.
pub(crate) const PROCESSING_QUIET_MS: f64 = 400.0;

/// The memory that keeps the chip's word from flickering between "Up to
/// date" (or the stream's word) and "Processing".
///
/// The raw [`ActivityState`] follows the worker pool frame by frame, and
/// during an ingest burst or a live stream the pool is busy for 50-200 ms at
/// a time with short gaps in between. The chip would follow that and flash.
/// Settling delays the switch into Processing until the work has gone on for
/// [`PROCESSING_ENTER_MS`], then keeps it for at least [`PROCESSING_HOLD_MS`]
/// and until the work has been quiet for [`PROCESSING_QUIET_MS`].
///
/// Only the quiet ⇄ Processing edge is settled. Downloading and Paused always
/// show at once, because they carry a count the user acted on, and a drained
/// download queue hands straight on to Processing with no wait.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct HeadlineSettle {
    /// When the current run of busy frames began, gaps under
    /// [`PROCESSING_QUIET_MS`] included.
    busy_since_ms: Option<f64>,
    /// The last frame the raw state was Processing.
    last_busy_ms: f64,
    /// When the chip started showing Processing, if it is.
    shown_since_ms: Option<f64>,
    /// What the chip showed last frame.
    shown: Option<ActivityState>,
}

impl HeadlineSettle {
    /// The state to show this frame, given the raw one. `quiet` is the state
    /// to show while Processing is held back: Streaming or UpToDate.
    pub(crate) fn step(
        &mut self,
        raw: ActivityState,
        quiet: ActivityState,
        now_ms: f64,
    ) -> ActivityState {
        let busy = raw == ActivityState::Processing;
        if busy {
            self.last_busy_ms = now_ms;
            self.busy_since_ms.get_or_insert(now_ms);
        } else if now_ms - self.last_busy_ms >= PROCESSING_QUIET_MS {
            self.busy_since_ms = None;
        }

        let showing = self.shown_since_ms.is_some();
        let shown = match raw {
            ActivityState::Downloading { .. } | ActivityState::Paused { .. } => raw,
            ActivityState::Processing => {
                let after_downloads = matches!(
                    self.shown,
                    Some(ActivityState::Downloading { .. } | ActivityState::Paused { .. })
                );
                let long_enough = self
                    .busy_since_ms
                    .is_some_and(|since| now_ms - since >= PROCESSING_ENTER_MS);
                if showing || after_downloads || long_enough {
                    ActivityState::Processing
                } else {
                    quiet
                }
            }
            ActivityState::UpToDate | ActivityState::Streaming => {
                let held = self.shown_since_ms.is_some_and(|since| {
                    now_ms - since < PROCESSING_HOLD_MS
                        || now_ms - self.last_busy_ms < PROCESSING_QUIET_MS
                });
                if held {
                    ActivityState::Processing
                } else {
                    raw
                }
            }
        };

        if shown == ActivityState::Processing {
            self.shown_since_ms.get_or_insert(now_ms);
        } else {
            self.shown_since_ms = None;
        }
        self.shown = Some(shown);
        shown
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
    let live_scans: Vec<(&str, i64)> = inputs
        .operations
        .iter()
        .filter(|op| matches!(op.status, OperationStatus::Queued | OperationStatus::Active))
        .filter_map(|op| match &op.kind {
            OperationKind::ArchiveDownload {
                site_id,
                scan_start,
                ..
            } => Some((site_id.as_str(), *scan_start)),
            _ => None,
        })
        .collect();

    inputs
        .awaiting
        .iter()
        .filter(|awaiting| {
            !live_scans.iter().any(|&(site, start)| {
                site == awaiting.site
                    && (start - awaiting.scan_start).abs() <= SCAN_JOIN_TOLERANCE_SECS
            })
        })
        .count() as u32
}

fn derive_state(
    counts: &StageCounts,
    inputs: &ActivityInputs<'_>,
    stream: Stream,
) -> ActivityState {
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
    if stream.headline_word().is_some() {
        return ActivityState::Streaming;
    }
    ActivityState::UpToDate
}

fn headline_for(
    state: ActivityState,
    counts: &StageCounts,
    stream: Stream,
    sites: SiteSum,
) -> ActivityHeadline {
    match state {
        ActivityState::UpToDate => ActivityHeadline {
            count: None,
            label: "Up to date",
            glyph: ActivityGlyph::Check,
            animate: false,
            sites,
        },
        ActivityState::Downloading { scans } => ActivityHeadline {
            count: Some(scans),
            label: "Downloading",
            glyph: ActivityGlyph::ArrowDown,
            animate: counts.downloading > 0,
            sites,
        },
        ActivityState::Processing => ActivityHeadline {
            count: None,
            label: "Processing",
            glyph: ActivityGlyph::Gear,
            animate: true,
            sites,
        },
        // The stream's own word, so the chip never says "Receiving" while
        // the stream line says it is waiting.
        ActivityState::Streaming => ActivityHeadline {
            count: None,
            label: stream.headline_word().unwrap_or("Receiving"),
            glyph: ActivityGlyph::Wave,
            animate: stream.is_animated(),
            sites,
        },
        ActivityState::Paused { queued } => ActivityHeadline {
            count: Some(queued),
            label: "Paused",
            glyph: ActivityGlyph::Pause,
            animate: false,
            sites,
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
    use crate::core::session::Stream;
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
        awaiting: Vec<AwaitingScan>,
        throughput: ThroughputWindow,
        aggregate: NetworkAggregate,
        requests: VecDeque<NetworkRequest>,
        queue_paused: bool,
        worker: WorkerLoad,
        last_worker_outcome_ms: f64,
        gpu_render_in_flight: bool,
        http_in_flight: u32,
        sites: Vec<SiteStream>,
        sheet_open: bool,
        dev: Option<ActivityDevInputs>,
        fps: Option<f64>,
        now_ms: f64,
        errors: crate::core::ErrorContext,
        errors_acked: u64,
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
                sites: vec![site("KDMX", Stream::Off)],
                sheet_open: false,
                dev: None,
                fps: None,
                now_ms: 1_000.0,
                errors: crate::core::ErrorContext::default(),
                errors_acked: 0,
            }
        }

        fn vm(&self) -> ActivityVm {
            ActivityVm::build(self.inputs())
        }

        fn inputs(&self) -> ActivityInputs<'_> {
            ActivityInputs {
                now_ms: self.now_ms,
                operations: &self.operations,
                queue_paused: self.queue_paused,
                ledger: LedgerSummary::default(),
                awaiting: &self.awaiting,
                worker: self.worker,
                last_worker_outcome_ms: self.last_worker_outcome_ms,
                gpu_render_in_flight: self.gpu_render_in_flight,
                throughput: &self.throughput,
                http_in_flight: self.http_in_flight,
                session_requests: 0,
                session_bytes: 0,
                sw_aggregate: &self.aggregate,
                fps: self.fps,
                recent_requests: &self.requests,
                cache_size_bytes: 0,
                sites: &self.sites,
                focus: 0,
                sheet_open: self.sheet_open,
                dev: self.dev.clone(),
                errors: &self.errors,
                errors_acked: self.errors_acked,
            }
        }
    }

    fn site(id: &str, stream: Stream) -> SiteStream {
        SiteStream {
            id: id.to_string(),
            stream,
            chunks_received: 0,
            last_chunk_age_secs: None,
            retry_in_secs: None,
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
        f.sites[0].stream = Stream::Receiving;
        assert_eq!(f.vm().state, ActivityState::Streaming);
        assert_eq!(f.vm().headline.label, "Receiving");
        assert!(f.vm().headline.animate);

        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        assert_eq!(f.vm().state, ActivityState::Downloading { scans: 1 });
    }

    /// A stalled stream is not "receiving" — it must not animate as if data
    /// were arriving.
    #[wasm_bindgen_test]
    fn stalled_stream_is_not_streaming_state() {
        let mut f = Fixture::new();
        f.sites[0].stream = Stream::Stalled;
        assert_eq!(f.vm().state, ActivityState::UpToDate);
        f.sites[0].stream = Stream::Failed;
        assert_eq!(f.vm().state, ActivityState::UpToDate);
    }

    /// The chip's stream word is the session's: a waiting stream says
    /// "Waiting" and holds still, so it never contradicts the stream line.
    #[wasm_bindgen_test]
    fn streaming_headline_projects_the_session_stream_word() {
        let mut f = Fixture::new();
        f.sites[0].stream = Stream::Waiting;
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Streaming);
        assert_eq!(vm.headline.label, "Waiting");
        assert!(!vm.headline.animate);

        f.sites[0].stream = Stream::Connecting;
        let vm = f.vm();
        assert_eq!(vm.headline.label, "Connecting");
        assert!(vm.headline.animate);
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
        f.awaiting = vec![awaiting("KDMX", 1000 + SCAN_JOIN_TOLERANCE_SECS - 1)];
        assert_eq!(f.vm().stages[3].count, 0);

        // A different scan, well outside the tolerance, does count.
        f.awaiting = vec![awaiting("KDMX", 9_000)];
        assert_eq!(f.vm().stages[3].count, 1);
    }

    fn awaiting(site: &str, scan_start: i64) -> AwaitingScan {
        AwaitingScan {
            site: site.into(),
            scan_start,
        }
    }

    /// Finishing sums every site's ledger, and another site's download at the
    /// same time does not cover a scan of this one.
    #[wasm_bindgen_test]
    fn settling_counts_every_site_and_matches_operations_by_site() {
        let mut f = Fixture::new();
        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        f.awaiting = vec![
            awaiting("KDMX", 1000),
            awaiting("KTLX", 1000),
            awaiting("KFWS", 4000),
        ];
        // KDMX's scan is still downloading; KTLX's and KFWS's are finishing.
        assert_eq!(f.vm().stages[3].count, 2);
    }

    /// With downloads drained, a settling scan keeps the app in Processing so
    /// the chip can't claim "up to date" mid-ingest.
    #[wasm_bindgen_test]
    fn settling_alone_reads_as_processing() {
        let mut f = Fixture::new();
        f.awaiting = vec![awaiting("KDMX", 1000)];
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
        f.awaiting = vec![awaiting("KDMX", 50_000)];
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
        f.dev = Some(ActivityDevInputs::default());
        let vm = f.vm();
        let detail = vm.detail.expect("developer rows");
        assert_eq!(detail.requests, 42);
        assert_eq!(detail.failed_requests, 2);
        assert_eq!(vm.session.bytes, 9_000);
    }

    /// Raw request counts are developer rows only (one-instrument §11):
    /// outside developer mode the Details carry bytes and cache, no counts.
    #[wasm_bindgen_test]
    fn request_counts_only_in_developer_mode() {
        let mut f = Fixture::new();
        f.aggregate = NetworkAggregate {
            total_requests: 42,
            failed_requests: 2,
            total_bytes: 9_000,
        };
        let vm = f.vm();
        assert!(vm.detail.is_none());
        assert_eq!(vm.session.bytes, 9_000);
        f.dev = Some(ActivityDevInputs::default());
        assert_eq!(f.vm().detail.map(|d| d.requests), Some(42));
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
                awaiting: &f.awaiting,
                worker: f.worker,
                last_worker_outcome_ms: f.last_worker_outcome_ms,
                gpu_render_in_flight: false,
                throughput: &f.throughput,
                http_in_flight: 0,
                session_requests: 0,
                session_bytes: 0,
                sw_aggregate: &f.aggregate,
                fps: None,
                recent_requests: &f.requests,
                cache_size_bytes: 0,
                sites: &f.sites,
                focus: 0,
                sheet_open: false,
                dev: Some(ActivityDevInputs::default()),
                errors: &f.errors,
                errors_acked: 0,
            }
        });
        assert_eq!(vm.detail.expect("developer rows").requests, 7);
        assert_eq!(vm.readout.requests, 7);
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
        f.dev = Some(ActivityDevInputs::default());
        let detail = f.vm().detail.expect("detail");
        assert_eq!(detail.http_in_flight, 4);
        assert_eq!(detail.worker.ingest, 2);
    }

    #[wasm_bindgen_test]
    fn dev_rows_list_each_sites_volume_texture() {
        use crate::core::volume_plan::VolumeTexture;
        let mut f = Fixture::new();
        f.dev = Some(ActivityDevInputs::default());
        assert!(f.vm().detail.expect("detail").volume_rows.is_empty());
        f.dev = Some(ActivityDevInputs {
            volume_textures: vec![
                VolumeTexture {
                    site: "KTBW".into(),
                    bytes: 6_000_000,
                },
                VolumeTexture {
                    site: "KMLB".into(),
                    bytes: 12_500_000,
                },
            ],
            ..ActivityDevInputs::default()
        });
        assert_eq!(
            f.vm().detail.expect("detail").volume_rows,
            vec![
                ("Volume KTBW".to_string(), "6.0 MB".to_string()),
                ("Volume KMLB".to_string(), "12 MB".to_string()),
            ]
        );
    }

    // ── Errors other than downloads ─────────────────────────────────────

    fn alerts_error(msg: &str) -> crate::core::AppError {
        crate::core::AppError::Alerts {
            message: msg.into(),
        }
    }

    fn download_error() -> crate::core::AppError {
        crate::core::AppError::Download {
            message: "404".into(),
            scan_start_secs: None,
        }
    }

    fn worker_error(site: Option<&str>) -> crate::core::AppError {
        crate::core::AppError::Worker {
            kind: crate::core::WorkerErrorKind::IdbFailure,
            message: "IDB write failed".into(),
            site: site.map(Into::into),
            scan_timestamp_secs: None,
        }
    }

    #[wasm_bindgen_test]
    fn the_sheet_names_an_errors_site_only_with_several_sites() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        f.errors.push(worker_error(Some("KMLB")));
        f.errors.push(worker_error(None));
        // One site: the words are the toast's.
        assert_eq!(
            f.vm().errors.recent,
            vec![
                "Decode worker: IDB write failed".to_string(),
                "Decode worker: IDB write failed".to_string(),
            ]
        );
        // Several: an error that knows its site leads with it.
        f.sites.push(site("KMLB", Stream::Off));
        assert_eq!(
            f.vm().errors.recent,
            vec![
                "Decode worker: IDB write failed".to_string(),
                "KMLB \u{00B7} Decode worker: IDB write failed".to_string(),
            ]
        );
    }

    #[wasm_bindgen_test]
    fn unseen_errors_count_on_the_chip_and_skip_downloads() {
        let mut f = Fixture::new();
        f.errors.push(alerts_error("timeout"));
        f.errors.push(download_error());
        f.errors.push(alerts_error("503"));
        let vm = f.vm();
        assert_eq!(vm.errors.unseen, 2);
        // The list is only built while the sheet is open.
        assert!(vm.errors.recent.is_empty());
    }

    #[wasm_bindgen_test]
    fn the_open_sheet_lists_errors_newest_first_and_acknowledges_them() {
        let mut f = Fixture::new();
        f.errors.push(alerts_error("first"));
        f.errors.push(alerts_error("second"));
        f.sheet_open = true;
        let vm = f.vm();
        assert_eq!(
            vm.errors.recent,
            vec!["NWS alerts: second", "NWS alerts: first"]
        );
        f.errors_acked = errors_acked(&f.errors, f.errors_acked, f.sheet_open);
        f.sheet_open = false;
        assert_eq!(f.vm().errors.unseen, 0);
        // A new one after the sheet closed counts again.
        f.errors.push(alerts_error("third"));
        assert_eq!(f.vm().errors.unseen, 1);
    }

    #[wasm_bindgen_test]
    fn a_closed_sheet_keeps_the_acknowledged_count() {
        let mut f = Fixture::new();
        f.errors.push(alerts_error("x"));
        assert_eq!(errors_acked(&f.errors, 0, false), 0);
        assert_eq!(errors_acked(&f.errors, 0, true), 1);
    }

    #[wasm_bindgen_test]
    fn unseen_errors_are_bounded_by_what_the_ring_still_holds() {
        let mut f = Fixture::new();
        for i in 0..80 {
            f.errors.push(alerts_error(&i.to_string()));
        }
        assert_eq!(f.vm().errors.unseen as usize, f.errors.len());
        f.sheet_open = true;
        assert_eq!(f.vm().errors.recent.len(), SHEET_ERRORS);
        assert_eq!(f.vm().errors.recent[0], "NWS alerts: 79");
    }

    #[wasm_bindgen_test]
    fn the_warning_tick_counts_failures_and_unseen_errors() {
        let mut f = Fixture::new();
        assert_eq!(f.vm().warning(), None);
        f.errors.push(alerts_error("x"));
        let (n, hover) = f.vm().warning().unwrap();
        assert_eq!(n, 1);
        assert_eq!(hover, "1 new error — click for details");
        f.errors_acked = 1;
        assert_eq!(f.vm().warning(), None);
    }

    // ── settling the headline ───────────────────────────────────────────────

    const P: ActivityState = ActivityState::Processing;
    const U: ActivityState = ActivityState::UpToDate;
    const S: ActivityState = ActivityState::Streaming;

    /// Steps `settle` through `(time ms, raw state)` frames and returns what
    /// was shown at each.
    fn run(settle: &mut HeadlineSettle, frames: &[(f64, ActivityState)]) -> Vec<ActivityState> {
        frames
            .iter()
            .map(|&(t, raw)| settle.step(raw, U, t))
            .collect()
    }

    /// Short decodes never reach the chip.
    #[wasm_bindgen_test]
    fn brief_processing_stays_up_to_date() {
        let mut settle = HeadlineSettle::default();
        let shown = run(
            &mut settle,
            &[(0.0, U), (16.0, P), (200.0, P), (400.0, U), (1_000.0, U)],
        );
        assert!(shown.iter().all(|&s| s == U), "{shown:?}");
    }

    /// Work that goes on past the enter delay shows, and then holds.
    #[wasm_bindgen_test]
    fn sustained_processing_shows_after_the_enter_delay() {
        let mut settle = HeadlineSettle::default();
        assert_eq!(settle.step(P, U, 0.0), U);
        assert_eq!(settle.step(P, U, PROCESSING_ENTER_MS - 1.0), U);
        assert_eq!(settle.step(P, U, PROCESSING_ENTER_MS), P);
        // Idle right away: still held for the minimum.
        let start = PROCESSING_ENTER_MS;
        assert_eq!(settle.step(U, U, start + 100.0), P);
        assert_eq!(settle.step(U, U, start + PROCESSING_HOLD_MS - 1.0), P);
        assert_eq!(settle.step(U, U, start + PROCESSING_HOLD_MS), U);
    }

    /// Busy bursts separated by short gaps count as one run of work, so the
    /// chip neither restarts its wait nor flashes back to idle.
    #[wasm_bindgen_test]
    fn short_gaps_do_not_flicker() {
        let mut settle = HeadlineSettle::default();
        let mut frames = Vec::new();
        // 120 ms busy, 80 ms idle, for six seconds.
        let mut t = 0.0;
        while t < 6_000.0 {
            frames.push((t, P));
            frames.push((t + 120.0, U));
            t += 200.0;
        }
        let shown = run(&mut settle, &frames);
        let switches = shown.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(switches, 1, "one switch into Processing: {shown:?}");
        assert_eq!(*shown.last().unwrap(), P);
    }

    /// A held Processing drops only after the work has been quiet a while,
    /// even when the minimum hold has long passed.
    #[wasm_bindgen_test]
    fn exit_waits_for_quiet() {
        let mut settle = HeadlineSettle::default();
        settle.step(P, U, 0.0);
        assert_eq!(settle.step(P, U, 1_000.0), P);
        assert_eq!(settle.step(P, U, 5_000.0), P);
        assert_eq!(settle.step(U, U, 5_000.0 + PROCESSING_QUIET_MS - 1.0), P);
        assert_eq!(settle.step(U, U, 5_000.0 + PROCESSING_QUIET_MS), U);
    }

    /// Downloading shows at once, and a drained queue hands straight on to
    /// Processing without the enter delay.
    #[wasm_bindgen_test]
    fn downloads_are_never_delayed() {
        let mut settle = HeadlineSettle::default();
        let d = ActivityState::Downloading { scans: 2 };
        assert_eq!(settle.step(U, U, 0.0), U);
        assert_eq!(settle.step(d, U, 10.0), d);
        assert_eq!(settle.step(P, U, 20.0), P);
        // Downloading again interrupts a held Processing.
        assert_eq!(settle.step(d, U, 30.0), d);
    }

    /// While a live stream runs, held-back Processing shows the stream's word.
    #[wasm_bindgen_test]
    fn streaming_is_the_quiet_word_while_live() {
        let mut settle = HeadlineSettle::default();
        assert_eq!(settle.step(S, S, 0.0), S);
        assert_eq!(settle.step(P, S, 100.0), S);
        assert_eq!(settle.step(S, S, 300.0), S);
    }

    /// The settled build swaps the headline along with the state, and leaves
    /// the sheet's stage counts raw.
    #[wasm_bindgen_test]
    fn build_settled_rewrites_the_headline() {
        let mut f = Fixture::new();
        f.worker = ingesting(1);
        f.sites[0].stream = Stream::Receiving;
        let mut settle = HeadlineSettle::default();
        let vm = ActivityVm::build_settled(f.inputs(), &mut settle);
        assert_eq!(vm.state, ActivityState::Streaming);
        assert_eq!(vm.headline.label, "Receiving");
        assert_eq!(vm.stages[2].count, 1);

        f.now_ms += PROCESSING_ENTER_MS;
        let vm = ActivityVm::build_settled(f.inputs(), &mut settle);
        assert_eq!(vm.state, ActivityState::Processing);
        assert_eq!(vm.headline.label, "Processing");
    }

    // ── readout ─────────────────────────────────────────────────────────────

    /// The readout is on outside dev mode, rounds the frame rate, and counts
    /// requests from the service worker when it has seen any.
    #[wasm_bindgen_test]
    fn readout_is_always_present() {
        let mut f = Fixture::new();
        assert_eq!(
            f.vm().readout,
            Readout {
                fps: None,
                requests: 0
            }
        );
        f.fps = Some(59.6);
        f.aggregate.total_requests = 12;
        assert!(f.vm().detail.is_none());
        assert_eq!(
            f.vm().readout,
            Readout {
                fps: Some(60),
                requests: 12
            }
        );
    }

    #[wasm_bindgen_test]
    fn readout_text() {
        let r = Readout {
            fps: Some(60),
            requests: 128,
        };
        assert_eq!(r.text(), "60 fps · 128 req");
        assert_eq!(Readout { fps: None, ..r }.text(), "128 req");
    }

    /// A bad frame-time sample never prints as a frame rate.
    #[wasm_bindgen_test]
    fn readout_drops_a_non_finite_fps() {
        let mut f = Fixture::new();
        f.fps = Some(f64::INFINITY);
        assert_eq!(f.vm().readout.fps, None);
    }

    // ── Sites: the chip's sum and the sheet's sections ──────────────────

    fn three_sites(f: &mut Fixture, streams: [Stream; 3]) {
        f.sites = ["KTBW", "KMLB", "KJAX"]
            .into_iter()
            .zip(streams)
            .map(|(id, s)| site(id, s))
            .collect();
    }

    #[wasm_bindgen_test]
    fn one_site_keeps_the_chip_words() {
        let mut f = Fixture::new();
        assert_eq!(f.vm().headline.words(), "Up to date");
        assert_eq!(f.vm().hover(), "Everything you're viewing is downloaded");
        f.sites[0].stream = Stream::Receiving;
        assert_eq!(f.vm().headline.words(), "Receiving");
        assert_eq!(f.vm().hover(), "Live stream running — click for details");
        // A stalled single site still reads as today: no sum, no count.
        f.sites[0].stream = Stream::Stalled;
        assert_eq!(f.vm().headline.words(), "Up to date");
    }

    #[wasm_bindgen_test]
    fn the_chip_sums_three_receiving_sites() {
        let mut f = Fixture::new();
        three_sites(&mut f, [Stream::Receiving; 3]);
        let vm = f.vm();
        assert_eq!(vm.headline.words(), "Receiving \u{00b7} 3 sites");
        assert_eq!(
            vm.hover(),
            "Live streams running on 3 sites — click for details"
        );
    }

    #[wasm_bindgen_test]
    fn the_chip_counts_a_stalled_site() {
        let mut f = Fixture::new();
        three_sites(
            &mut f,
            [Stream::Receiving, Stream::Waiting, Stream::Stalled],
        );
        let vm = f.vm();
        assert_eq!(vm.headline.words(), "Receiving 3 sites \u{00b7} 1 stalled");
        assert_eq!(
            vm.hover(),
            "Live streams running on 3 sites, 1 stream stalled — click for details"
        );
    }

    #[wasm_bindgen_test]
    fn the_chip_reads_up_to_date_over_three_quiet_sites() {
        let mut f = Fixture::new();
        three_sites(&mut f, [Stream::Off; 3]);
        let vm = f.vm();
        assert_eq!(vm.headline.words(), "Up to date \u{00b7} 3 sites");
        assert_eq!(
            vm.hover(),
            "Everything you're viewing on 3 sites is downloaded"
        );
    }

    #[wasm_bindgen_test]
    fn the_sum_rides_on_downloads_too() {
        let mut f = Fixture::new();
        three_sites(&mut f, [Stream::Off; 3]);
        f.operations
            .push_back(download(1, 1000, OperationStatus::Active));
        let vm = f.vm();
        assert_eq!(vm.headline.count, Some(1));
        assert_eq!(vm.headline.words(), "Downloading \u{00b7} 3 sites");
    }

    /// One quiet site never hides another that is receiving: the chip
    /// speaks for the healthiest stream.
    #[wasm_bindgen_test]
    fn the_headline_stream_is_the_healthiest_site() {
        let pick = |streams: &[Stream]| {
            let sites: Vec<SiteStream> = streams.iter().map(|s| site("K", *s)).collect();
            headline_stream(&sites)
        };
        assert_eq!(
            pick(&[Stream::Stalled, Stream::Receiving]),
            Stream::Receiving
        );
        assert_eq!(
            pick(&[Stream::Waiting, Stream::Connecting]),
            Stream::Connecting
        );
        assert_eq!(pick(&[Stream::Off, Stream::Waiting]), Stream::Waiting);
        assert_eq!(pick(&[Stream::Off, Stream::Stalled]), Stream::Stalled);
        assert_eq!(pick(&[Stream::Failed]), Stream::Failed);
        assert_eq!(pick(&[]), Stream::Off);
    }

    #[wasm_bindgen_test]
    fn the_sheet_has_no_site_sections_with_one_site() {
        let mut f = Fixture::new();
        f.sheet_open = true;
        assert!(f.vm().sites.is_empty());
    }

    #[wasm_bindgen_test]
    fn the_sheet_lists_the_focus_first_then_list_order() {
        let mut f = Fixture::new();
        three_sites(
            &mut f,
            [Stream::Receiving, Stream::Waiting, Stream::Stalled],
        );
        f.sheet_open = true;
        let mut inputs = f.inputs();
        inputs.focus = 1;
        let vm = ActivityVm::build(inputs);
        let ids: Vec<_> = vm.sites.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["KMLB", "KTBW", "KJAX"]);
        assert!(vm.sites[0].focus);
        assert!(!vm.sites[1].focus && !vm.sites[2].focus);
        assert!(vm.sites[2].unhealthy);
        assert!(!vm.sites[0].unhealthy);
    }

    #[wasm_bindgen_test]
    fn the_sheet_sections_are_built_only_while_it_is_open() {
        let mut f = Fixture::new();
        three_sites(&mut f, [Stream::Off; 3]);
        assert!(f.vm().sites.is_empty());
    }

    #[wasm_bindgen_test]
    fn a_site_section_reads_its_stream_chunks_and_last_chunk_age() {
        let mut s = site("KMLB", Stream::Receiving);
        s.chunks_received = 12;
        s.last_chunk_age_secs = Some(4.2);
        assert_eq!(
            site_stream_line(&s),
            "Receiving \u{00b7} 12 chunks \u{00b7} last 0:04 ago"
        );
        s.chunks_received = 1;
        s.last_chunk_age_secs = None;
        s.stream = Stream::Stalled;
        assert_eq!(site_stream_line(&s), "Stalled \u{00b7} 1 chunk");
        assert_eq!(
            site_stream_line(&site("KJAX", Stream::Off)),
            "Not streaming"
        );
        assert_eq!(
            site_stream_line(&site("KJAX", Stream::Failed)),
            "Stream failed"
        );
        let mut retrying = site("KJAX", Stream::Retrying);
        retrying.retry_in_secs = Some(11.2);
        assert_eq!(
            site_stream_line(&retrying),
            "Stream failed \u{00b7} retrying in 0:12"
        );
        retrying.retry_in_secs = Some(0.0);
        assert_eq!(
            site_stream_line(&retrying),
            "Stream failed \u{00b7} retrying"
        );
    }

    /// A failed stream that will start again says so on the chip, and holds
    /// still: nothing is arriving.
    #[wasm_bindgen_test]
    fn a_retrying_stream_reads_reconnecting() {
        let mut f = Fixture::new();
        f.sites[0].stream = Stream::Retrying;
        let vm = f.vm();
        assert_eq!(vm.state, ActivityState::Streaming);
        assert_eq!(vm.headline.label, "Reconnecting");
        assert!(!vm.headline.animate);
    }

    #[wasm_bindgen_test]
    fn a_site_section_counts_only_its_own_failed_downloads() {
        let mut f = Fixture::new();
        three_sites(&mut f, [Stream::Off; 3]);
        let mut failed = download(
            1,
            1000,
            OperationStatus::Failed {
                error: "HTTP 503".into(),
            },
        );
        if let OperationKind::ArchiveDownload { site_id, .. } = &mut failed.kind {
            *site_id = "KJAX".into();
        }
        f.operations.push_back(failed);
        let sections = site_sections(&f.sites, 0, &f.operations);
        assert_eq!(sections[0].failed, 0);
        assert_eq!(sections[2].id, "KJAX");
        assert_eq!(sections[2].failed, 1);
        assert_eq!(sections[2].first_error.as_deref(), Some("HTTP 503"));
    }

    /// Every state explains itself on hover, with one site and with three.
    #[wasm_bindgen_test]
    fn every_state_has_hover_text_for_one_site_and_many() {
        for many in [false, true] {
            let mut f = Fixture::new();
            if many {
                three_sites(&mut f, [Stream::Off; 3]);
            }
            let mut hovers = vec![f.vm().hover()];
            f.gpu_render_in_flight = true;
            hovers.push(f.vm().hover());
            f.gpu_render_in_flight = false;
            f.sites[0].stream = Stream::Receiving;
            hovers.push(f.vm().hover());
            f.operations
                .push_back(download(1, 1000, OperationStatus::Queued));
            hovers.push(f.vm().hover());
            f.queue_paused = true;
            hovers.push(f.vm().hover());
            assert!(hovers.iter().all(|h| !h.is_empty()));
            let distinct: std::collections::BTreeSet<_> = hovers.iter().collect();
            assert_eq!(distinct.len(), 5, "{hovers:?}");
            assert_eq!(hovers.iter().all(|h| h.contains("3 sites")), many);
        }
    }
}
