//! `TimelineView` — the single source-agnostic view the timeline UI renders
//! from, produced by one adapter that merges every timeline data source.
//!
//! Before this module the timeline had a *split-brain*: cached scans
//! (`RadarTimeline`) and the in-progress live volume (`VcpPositionModel`)
//! were drawn by different renderers that *filtered each other out* instead
//! of *merging*. A scan that was partially downloaded and then resumed live
//! was handed wholesale to the live overlay, which had no idea its cached
//! sweeps existed — so they vanished from the timeline until streaming
//! stopped. The old `TimelineModel` only did dedup-by-filtering
//! ("renderer, skip this one"); it never merged per sweep.
//!
//! `TimelineView` replaces that. It is built once per frame from the sources
//! passed by reference — the IDB cache, the archive shadow boundaries, the
//! live streaming state, and its projection — none of which reference each
//! other. The UI asks the view *availability* questions ("what's cached?
//! what's collecting? is this point already covered?") and never reaches
//! past it to a raw source. The cache↔live merge lives in exactly one place
//! ([`merge_cached_into_live`]), which makes the disappearing-sweep bug
//! structurally impossible: a streaming volume keeps its cached sweeps
//! ([`SweepAvailability::Cached`]) while its active cut reads as
//! [`SweepAvailability::Collecting`].
//!
//! Like its predecessor the view *borrows* the cache scans (the common,
//! large source) to avoid cloning them every frame, and *owns* only the
//! small, merged in-progress piece (the live volume's position model). The
//! borrowing keeps per-frame cost negligible while still routing every read
//! through one handle.
//!
//! On top of the availability questions, the view also produces the
//! **frame-cell join** ([`TimelineView::frame_containers_in_range`]): the
//! single place that merges cache + shadows + download progress + acquisition
//! failures + the live projection into per-cell states the frames-first strip
//! renders (spec §6.3).

use crate::data::ScanKey;
use crate::nexrad::projection::{ScanProjection, SweepProjectionStatus, SweepTimingProvenance};
use crate::nexrad::ScanBoundary;
use crate::state::radar_data::{RadarTimeline, Scan};
use crate::state::LiveModeState;
use std::collections::BTreeSet;

/// The single tolerance for matching one scan-start second against another
/// across every timeline source — cached scans, archive/shadow boundaries,
/// download-progress ranges, failed-scan timestamps, and the frame-cell join.
/// Wider than zero because a boundary comes from the archive listing's
/// filename time while a cached scan's `key_timestamp` comes from the
/// volume-header time parsed during ingest; the two can differ by a few
/// seconds without representing different volumes. 60s comfortably covers
/// normal NEXRAD volume cadence (~5–10 min) without absorbing adjacent
/// volumes. (The former `SCAN_CACHE_MATCH_TOLERANCE_SECS` in `main.rs` now
/// aliases this so there is one number.)
pub const SCAN_JOIN_TOLERANCE_SECS: i64 = 60;

/// Back-compat alias for [`SCAN_JOIN_TOLERANCE_SECS`]; the boundary-match name
/// existed before the join consolidated every scan-start tolerance here.
pub const BOUNDARY_MATCH_TOLERANCE_SECS: i64 = SCAN_JOIN_TOLERANCE_SECS;

// ──────────────────────────────────────────────────────────────────────────
// Frame-cell state model (spec §6.2 / §6.3 frames-first).
//
// A *frame* is a sweep matching the currently selected product + tilt — the
// thing the canvas can render. The strip's primary unit is the frame cell;
// the full volume structure is only a faint sub-texture inside each scan
// container. This model is the JOIN of the four previously-uncorrelated
// sources (cache, archive shadows, download progress, acquisition failures,
// and the live projection) onto one per-cell state, keyed by scan-start
// seconds within [`SCAN_JOIN_TOLERANCE_SECS`].
// ──────────────────────────────────────────────────────────────────────────

/// One frame cell's acquisition/display state. Distinguishable by fill + shape
/// in grayscale (spec §6.2 accessibility): hollow outline, solid fill, segmented
/// in-flight, hatched queued, dashed ghost, alert-tick failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameCellState {
    /// Downloaded and renderable — solid fill.
    Cached,
    /// Published on the server but not downloaded — hollow outline. For
    /// shadow/undownloaded scans the selected tilt is assumed to exist.
    Available,
    /// A download/ingest is running for this scan — segmented (live chunks) or
    /// pulsing (archive) fill.
    InFlight,
    /// Queued to download — faint hatch texture.
    Queued,
    /// Future ghost (live projection only) — dashed ghost at the predicted time.
    Projected,
    /// A download failed for this scan — small red alert tick; click to retry.
    Failed,
}

/// Per-chunk segmentation for an in-flight frame cell driven by live chunk
/// telemetry (3 or 6 slots, faithfully — alignment decision #4). Archive
/// in-flight cells carry `None` here and use a pulsing fill instead.
#[derive(Clone, Debug, Default)]
pub struct CellChunkProgress {
    /// Total chunk slots expected in this sweep (3 or 6; 0 ⇒ unknown).
    pub chunks_expected: u32,
    /// Whole chunks received so far.
    pub chunks_received: u32,
    /// Radials accumulated in the currently-filling (partial) chunk.
    pub partial_radials: u32,
}

/// One frame cell: a sweep matching the selected product + tilt (a SAILS
/// revisit means a scan can hold multiple). Carries its own time span, tilt
/// identity, join-resolved state, and (for in-flight live cells) chunk
/// segmentation.
#[derive(Clone, Debug)]
pub struct FrameCell {
    /// Collection-time span (radar physically scans). Drives x-extent.
    pub start_secs: f64,
    pub end_secs: f64,
    /// 1-based elevation number of the matched tilt.
    pub elevation_number: u8,
    /// Display elevation angle (degrees) — VCP target where known.
    pub elevation_angle: f32,
    /// Join-resolved acquisition state.
    pub state: FrameCellState,
    /// Live chunk segmentation when this cell is being collected with chunk
    /// telemetry; `None` for archive in-flight (pulse) and all settled cells.
    pub chunks: Option<CellChunkProgress>,
    /// Whether this is the cell currently on the GPU (drives the accent ring).
    pub is_active: bool,
    /// Whether this was the previous on-GPU cell (faint ring during blend).
    pub is_prev_active: bool,
}

/// A scan container: a subtle bounding box per volume scan, holding the frame
/// cells of the selected product/tilt plus the faint neutral sub-texture of
/// the full volume's sweep boundaries.
#[derive(Clone, Debug)]
pub struct ScanContainer {
    /// Container left edge (scan start, seconds).
    pub start_secs: f64,
    /// Container right edge — the clamped display end (next-volume start).
    pub end_secs: f64,
    /// The scan's storage-key timestamp (seconds) — the join + hit-test key.
    pub key_secs: f64,
    /// VCP number (0 ⇒ unknown). Carried for tooltips / labels, not color.
    pub vcp: u16,
    /// Frame cells (selected product+tilt) inside this container, time-sorted.
    pub cells: Vec<FrameCell>,
    /// Faint sub-texture: collection-time boundaries of *every* sweep in the
    /// volume (not just matching tilts), as `(start, end)` second spans, for
    /// the thin neutral interior lines. Empty when the structure is unknown.
    pub sweep_spans: Vec<(f64, f64)>,
    /// True when the whole scan is server-only (no cached sweeps) — the
    /// container itself reads as available, and its cells are `Available`.
    pub is_available: bool,
    /// True when this container is the live in-progress volume.
    pub is_live: bool,
}

impl ScanContainer {
    /// Whether `ts` falls within the container's displayed span. The hit-test
    /// entry point Phase 3's inspector/scrub-to-cell will read.
    #[allow(dead_code)]
    pub fn contains(&self, ts: f64) -> bool {
        ts >= self.start_secs && ts <= self.end_secs
    }
}

/// The extra join inputs the frame-cell model needs beyond the sources
/// [`TimelineView`] already holds: the download-progress ghost ranges and the
/// set of failed scan-starts. Kept as a small borrowed bundle so the renderer
/// passes them in one place and the view owns the join.
#[derive(Clone, Copy)]
pub struct FrameJoinInputs<'i> {
    /// `(start, end)` second spans queued to download.
    pub queued: &'i [(i64, i64)],
    /// `(start, end)` second spans actively downloading or ingesting.
    pub in_flight: &'i [(i64, i64)],
    /// Scan-start seconds whose download failed (from `AppError::Download`).
    pub failed: &'i [i64],
    /// Selected product (worker-string) — a cell counts as Cached only when the
    /// sweep holds a blob for this product.
    pub product: &'i str,
    /// Selected tilt (1-based elevation number); `None` ⇒ Latest/auto (every
    /// elevation is a frame).
    pub tilt: Option<u8>,
    /// On-GPU `(scan_key_secs, elevation_number)` for the active-ring cell.
    pub active: Option<(f64, u8)>,
    /// Previous on-GPU `(scan_key_secs, elevation_number)` during blend.
    pub prev_active: Option<(f64, u8)>,
}

/// Frame-scoped, source-agnostic view of the timeline.
///
/// Borrows the cache scans; owns the merged in-progress volume.
pub struct TimelineView<'a> {
    cache: &'a RadarTimeline,
    shadows: &'a [ScanBoundary],
    /// Key-millis of the in-progress volume the overlay will draw. Excluded
    /// from the "settled" (cached-track) iterators so it isn't drawn twice.
    /// `None` when there is no live overlay (then the cached scan, if any,
    /// renders normally on the settled track).
    live_volume_ms: Option<i64>,
    /// The in-progress volume, with cached sweeps merged in. Owned.
    live_position: Option<ScanProjection>,
    /// Cached scan key-millis (plus the live volume) for O(log n) coverage
    /// queries by the shadow/ghost overlays.
    coverage_keys: BTreeSet<i64>,
    elevation_filter: Option<u8>,
    now_secs: f64,
}

impl<'a> TimelineView<'a> {
    /// Build the view from every timeline source. Sources are passed by
    /// reference and never reference each other; all merging happens here.
    ///
    /// `live_position` is the live subsystem's per-frame `from_live` model
    /// (`live.radar_model.position`); the adapter clones it and overlays the
    /// cached sweeps for the same volume.
    pub fn build(
        cache: &'a RadarTimeline,
        shadows: &'a [ScanBoundary],
        live_state: Option<&LiveModeState>,
        live_position: Option<&ScanProjection>,
        elevation_filter: Option<u8>,
        now_secs: f64,
    ) -> Self {
        let (live_position, live_volume_ms) = match (live_state, live_position) {
            (Some(ls), Some(pos)) if ls.is_active() => {
                let anchor_ms = ls.current_volume.as_ref().map(|a| a.scan_key.scan_start.0);
                let mut merged = pos.clone();
                if let Some(ms) = anchor_ms {
                    if let Some(scan) = scan_with_key_ms(cache, ms) {
                        merge_cached_into_live(&mut merged, scan);
                    }
                }
                (Some(merged), anchor_ms)
            }
            _ => (None, None),
        };

        // Coverage set: every cached scan key, plus the live volume (which is
        // also "data we have / are getting" for ghost/shadow suppression).
        let mut coverage_keys: BTreeSet<i64> = cache.scans.iter().map(|s| s.key_ms()).collect();
        if let Some(ms) = live_volume_ms {
            coverage_keys.insert(ms);
        }

        Self {
            cache,
            shadows,
            live_volume_ms,
            live_position,
            coverage_keys,
            elevation_filter,
            now_secs,
        }
    }

    /// Every cached scan whose *displayed* block intersects `[start, end]`,
    /// paired with that block's clamped right edge (see [`clamped_display_end`]).
    /// Includes the in-progress volume's cached scan — connector lines want it;
    /// [`Self::settled_scans_in_range`] filters it back out for the settled
    /// track. This is the single source of the displayed right edge, so block
    /// fills, connector lines, and visual-range culling all agree on one value.
    pub fn visual_scans_in_range(
        &self,
        start: f64,
        end: f64,
    ) -> impl Iterator<Item = (&'a Scan, f64)> {
        let scans: &'a [Scan] = &self.cache.scans;
        let shadows: &'a [ScanBoundary] = self.shadows;
        scans.iter().enumerate().filter_map(move |(i, scan)| {
            let clamped_end = clamped_display_end(scans, shadows, i);
            (clamped_end >= start && scan.start_time <= end).then_some((scan, clamped_end))
        })
    }

    /// Cached ("settled") scans overlapping `[start, end]`, excluding the
    /// in-progress volume (the realtime overlay owns that). These carry
    /// [`crate::state::SweepAvailability::Cached`] availability. Borrows tie
    /// to the underlying cache (`'a`), not to the view, so callers can return
    /// them past the view's own scope. Each item carries the clamped display
    /// end from [`Self::visual_scans_in_range`].
    pub fn settled_scans_in_range(
        &self,
        start: f64,
        end: f64,
    ) -> impl Iterator<Item = (&'a Scan, f64)> {
        let live_ms = self.live_volume_ms;
        self.visual_scans_in_range(start, end)
            .filter(move |(s, _)| !is_live_scan(s, live_ms))
    }

    /// The cached scan at `ts`, excluding the in-progress volume.
    pub fn settled_scan_at(&self, ts: f64) -> Option<&'a Scan> {
        let scan = self.cache.find_scan_at_timestamp(ts)?;
        (!is_live_scan(scan, self.live_volume_ms)).then_some(scan)
    }

    /// The full cache handle (for callers that need range bounds or sweep
    /// lookups across all cached scans, e.g. the mobile scrubber). Prefer
    /// the availability-oriented accessors above where possible.
    pub fn cache(&self) -> &'a RadarTimeline {
        self.cache
    }

    /// Archive shadow boundaries (scans known to exist but not downloaded).
    pub fn shadow_boundaries(&self) -> &'a [ScanBoundary] {
        self.shadows
    }

    /// The in-progress volume, with cached sweeps merged in. `None` when not
    /// streaming (or when the VCP isn't known yet and no overlay is drawn).
    pub fn live_volume(&self) -> Option<&ScanProjection> {
        self.live_position.as_ref()
    }

    /// Whether a download-ghost or shadow-boundary at `start_secs` is already
    /// covered by cached data (or the in-progress volume), within
    /// [`BOUNDARY_MATCH_TOLERANCE_SECS`]. Overlays use this to suppress
    /// markers for ranges already represented by a filled block.
    pub fn is_covered_by_cached(&self, start_secs: i64) -> bool {
        let lo = (start_secs - BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        let hi = (start_secs + BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        self.coverage_keys.range(lo..=hi).next().is_some()
    }

    /// The frame's active tilt filter (1-based elevation number), carried for
    /// consumers that key off it; the frame-cell join takes the tilt explicitly
    /// via [`FrameJoinInputs`] instead.
    #[allow(dead_code)]
    pub fn elevation_filter(&self) -> Option<u8> {
        self.elevation_filter
    }

    #[allow(dead_code)] // Carried for consumers that need the frame's canonical `now`.
    pub fn now_secs(&self) -> f64 {
        self.now_secs
    }

    // ──────────────────────────────────────────────────────────────────────
    // Frame-cell join (spec §6.3 frames-first). One method merges every
    // source onto per-cell states keyed by scan-start seconds within
    // SCAN_JOIN_TOLERANCE_SECS. Renderers read ONLY these containers — they
    // never re-correlate the raw sources themselves.
    // ──────────────────────────────────────────────────────────────────────

    /// Build the scan containers (with their frame cells) overlapping
    /// `[start, end]`, for the Micro/frames-first strip. The selected
    /// product/tilt and the download/failure inputs come from `join`.
    /// Containers are returned in time order: settled (cached + available)
    /// first, then the live in-progress volume last so it paints on top.
    pub fn frame_containers_in_range(
        &self,
        start: f64,
        end: f64,
        join: FrameJoinInputs<'_>,
    ) -> Vec<ScanContainer> {
        let mut containers: Vec<ScanContainer> = Vec::new();

        // 1. Cached ("settled") scans → containers with cached cells.
        for (scan, clamped_end) in self.settled_scans_in_range(start, end) {
            containers.push(self.container_from_cached(scan, clamped_end, &join));
        }

        // 2. Archive shadow boundaries not covered by cache → Available
        //    containers (one assumed frame cell for the selected tilt).
        for b in self.shadows {
            if self.is_covered_by_cached(b.start) {
                continue;
            }
            if (b.end as f64) < start || (b.start as f64) > end {
                continue;
            }
            containers.push(container_from_shadow(b, &join));
        }

        // 3. The live in-progress volume → its own container (chunk-segmented
        //    matching cell + projected ghosts). Its next-scan ghost becomes a
        //    fully-Projected container so the next-volume cells read as ghosts
        //    and the nearest-ghost countdown logic can target them.
        if let Some(pos) = self.live_position.as_ref() {
            if pos.volume_end >= start && pos.volume_start <= end {
                containers.push(container_from_live(pos, &join));
            }
            if let Some(ghost) = pos.next_scan_ghost.as_deref() {
                if ghost.volume_end >= start && ghost.volume_start <= end {
                    containers.push(container_from_ghost(ghost, &join));
                }
            }
        }

        containers.sort_by(|a, b| {
            a.start_secs
                .partial_cmp(&b.start_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Live volume paints last among equal starts.
                .then(a.is_live.cmp(&b.is_live))
        });
        containers
    }

    /// Build a container from a cached scan, resolving each matching cell's
    /// state against the download/failure inputs (a cached sweep that is also
    /// mid-refresh reads InFlight; a failed scan-start tags its cells Failed).
    fn container_from_cached(
        &self,
        scan: &Scan,
        clamped_end: f64,
        join: &FrameJoinInputs<'_>,
    ) -> ScanContainer {
        let key_secs = scan.key_timestamp;
        let key_i = key_secs.round() as i64;
        let in_flight = range_hits(join.in_flight, key_i);
        let failed = secs_hit(join.failed, key_i);

        let sweep_spans: Vec<(f64, f64)> = scan
            .sweeps
            .iter()
            .map(|s| (s.start_time, s.end_time))
            .collect();

        let mut cells: Vec<FrameCell> = Vec::new();
        for sw in &scan.sweeps {
            if !tilt_matches(join.tilt, sw.elevation_number) {
                continue;
            }
            let has_product = sw.cached_products.iter().any(|p| p == join.product)
                // An empty/unknown cached set ("legacy index entry") still
                // counts as on-device for cell purposes — the scan downloaded.
                || sw.cached_products.is_empty();
            // A cached sweep with the product blob is downloaded → Cached. A
            // matching tilt without this product reads InFlight while its scan
            // is mid-fetch, Failed if its scan's download failed, else Available
            // (the volume exists but not this product locally).
            let state = if has_product {
                FrameCellState::Cached
            } else if in_flight {
                FrameCellState::InFlight
            } else if failed {
                FrameCellState::Failed
            } else {
                FrameCellState::Available
            };
            cells.push(self.make_cell(scan, sw, state, None, join));
        }

        // A scan indexed but with no cached sweeps (completeness Missing) is
        // semantically "available, not downloaded" — match the Macro track and
        // the old `draw_available_block`. Render the container hollow and, when
        // it produced no cells (its sweeps are empty), surface one assumed
        // Available cell for the selected tilt so the container reads as a
        // tappable frame, not an empty box.
        let is_available =
            scan.completeness == Some(crate::data::ScanCompleteness::Missing) && cells.is_empty();
        if is_available {
            let state = if in_flight {
                FrameCellState::InFlight
            } else if failed {
                FrameCellState::Failed
            } else {
                FrameCellState::Available
            };
            cells.push(FrameCell {
                start_secs: scan.start_time,
                end_secs: clamped_end,
                elevation_number: join.tilt.unwrap_or(0),
                elevation_angle: 0.0,
                state,
                chunks: None,
                is_active: false,
                is_prev_active: false,
            });
        }

        ScanContainer {
            start_secs: scan.start_time,
            end_secs: clamped_end,
            key_secs,
            vcp: scan.vcp,
            cells,
            sweep_spans,
            is_available,
            is_live: false,
        }
    }

    /// Build a frame cell from a cached sweep, attaching the active-ring flags.
    fn make_cell(
        &self,
        scan: &Scan,
        sw: &crate::state::radar_data::Sweep,
        state: FrameCellState,
        chunks: Option<CellChunkProgress>,
        join: &FrameJoinInputs<'_>,
    ) -> FrameCell {
        let (is_active, is_prev_active) = ring_flags(join, scan.key_timestamp, sw.elevation_number);
        FrameCell {
            start_secs: sw.start_time,
            end_secs: sw.end_time,
            elevation_number: sw.elevation_number,
            elevation_angle: scan.display_angle(sw),
            state,
            chunks,
            is_active,
            is_prev_active,
        }
    }
}

// ── Frame-cell join helpers (free functions; pure, unit-tested) ────────────

/// Whether a selected tilt matches an elevation number. `None` ⇒ Latest/auto,
/// where every elevation is a frame.
fn tilt_matches(tilt: Option<u8>, elevation_number: u8) -> bool {
    tilt.is_none_or(|t| t == elevation_number)
}

/// Whether any `(start, end)` range in `ranges` contains the scan-start
/// `key_i` within [`SCAN_JOIN_TOLERANCE_SECS`] of its start.
fn range_hits(ranges: &[(i64, i64)], key_i: i64) -> bool {
    ranges
        .iter()
        .any(|&(s, _)| (s - key_i).abs() <= SCAN_JOIN_TOLERANCE_SECS)
}

/// Whether any failed scan-start in `failed` matches `key_i` within tolerance.
fn secs_hit(failed: &[i64], key_i: i64) -> bool {
    failed
        .iter()
        .any(|&s| (s - key_i).abs() <= SCAN_JOIN_TOLERANCE_SECS)
}

/// `(is_active, is_prev_active)` ring flags for a `(scan_key_secs, elevation)`
/// cell against the on-GPU identity carried in the join inputs.
fn ring_flags(join: &FrameJoinInputs<'_>, key_secs: f64, elev: u8) -> (bool, bool) {
    let matches =
        |o: Option<(f64, u8)>| o.is_some_and(|(ts, en)| (ts - key_secs).abs() < 0.5 && en == elev);
    let is_active = matches(join.active);
    let is_prev_active = !is_active && matches(join.prev_active);
    (is_active, is_prev_active)
}

/// Ring flags keyed by ELEVATION only — for the live volume, which is unique,
/// so the on-GPU cut is identified by its elevation without a timestamp match
/// (whose tolerance the collection-adjusted `volume_start` can exceed).
fn ring_flags_by_elevation(join: &FrameJoinInputs<'_>, elev: u8) -> (bool, bool) {
    let matches = |o: Option<(f64, u8)>| o.is_some_and(|(_, en)| en == elev);
    let is_active = matches(join.active);
    let is_prev_active = !is_active && matches(join.prev_active);
    (is_active, is_prev_active)
}

/// Build an Available container from an archive shadow boundary. The selected
/// tilt is assumed to exist (one hollow cell); download/failure inputs upgrade
/// it to InFlight / Queued / Failed as appropriate.
fn container_from_shadow(b: &ScanBoundary, join: &FrameJoinInputs<'_>) -> ScanContainer {
    let key_i = b.start;
    let state = if range_hits(join.in_flight, key_i) {
        FrameCellState::InFlight
    } else if range_hits(join.queued, key_i) {
        FrameCellState::Queued
    } else if secs_hit(join.failed, key_i) {
        FrameCellState::Failed
    } else {
        FrameCellState::Available
    };
    // One assumed cell for the selected tilt spanning the container; in Latest
    // mode the tilt is unknown, so the single cell carries elevation 0.
    let cell = FrameCell {
        start_secs: b.start as f64,
        end_secs: b.end as f64,
        elevation_number: join.tilt.unwrap_or(0),
        elevation_angle: 0.0,
        state,
        chunks: None,
        is_active: false,
        is_prev_active: false,
    };
    ScanContainer {
        start_secs: b.start as f64,
        end_secs: b.end as f64,
        key_secs: b.start as f64,
        vcp: 0,
        cells: vec![cell],
        sweep_spans: Vec::new(),
        is_available: true,
        is_live: false,
    }
}

/// Build the live in-progress volume's container: a chunk-segmented (or
/// pulsing) matching cell for the collecting cut, cached matching cells, and
/// dashed projected ghosts for matching tilts not yet collected. Non-matching
/// sweeps contribute only to `sweep_spans` (the faint sub-texture).
fn container_from_live(pos: &ScanProjection, join: &FrameJoinInputs<'_>) -> ScanContainer {
    let key_secs = pos.volume_start;
    let sweep_spans: Vec<(f64, f64)> = pos
        .sweeps
        .iter()
        .map(|s| (s.collection_start_secs, s.collection_end_secs))
        .collect();

    let mut cells: Vec<FrameCell> = Vec::new();
    for sp in &pos.sweeps {
        if !tilt_matches(join.tilt, sp.elevation_number) {
            continue;
        }
        let (state, chunks) = match sp.status {
            SweepProjectionStatus::CollectedByUs => (FrameCellState::Cached, None),
            SweepProjectionStatus::InProgress => (
                FrameCellState::InFlight,
                Some(CellChunkProgress {
                    chunks_expected: sp.chunks_in_sweep as u32,
                    chunks_received: sp.chunks_received,
                    partial_radials: pos.in_progress_radials.unwrap_or(0),
                }),
            ),
            SweepProjectionStatus::AvailableNotCollected => (FrameCellState::Available, None),
            SweepProjectionStatus::FutureExpected => (FrameCellState::Projected, None),
        };
        // The live volume is unique, so its on-GPU cut is identified by
        // ELEVATION alone — `pos.volume_start` (collection-adjusted) can drift
        // from the displayed cut's storage-key timestamp by more than the ring
        // tolerance, which would otherwise drop the active highlight while
        // streaming.
        let (is_active, is_prev_active) = ring_flags_by_elevation(join, sp.elevation_number);
        cells.push(FrameCell {
            start_secs: sp.collection_start_secs,
            end_secs: sp.collection_end_secs,
            elevation_number: sp.elevation_number,
            elevation_angle: sp.elevation_angle,
            state,
            chunks,
            is_active,
            is_prev_active,
        });
    }

    ScanContainer {
        start_secs: pos.volume_start,
        end_secs: pos.volume_end,
        key_secs,
        vcp: pos.vcp_number,
        cells,
        sweep_spans,
        is_available: false,
        is_live: true,
    }
}

/// Build a Projected container from the next-scan ghost (the predicted next
/// volume during live streaming). Every matching cell is `Projected` so it
/// reads as a dashed ghost; the nearest-ghost countdown targets it.
fn container_from_ghost(ghost: &ScanProjection, join: &FrameJoinInputs<'_>) -> ScanContainer {
    let sweep_spans: Vec<(f64, f64)> = ghost
        .sweeps
        .iter()
        .map(|s| (s.collection_start_secs, s.collection_end_secs))
        .collect();
    let cells: Vec<FrameCell> = ghost
        .sweeps
        .iter()
        .filter(|sp| tilt_matches(join.tilt, sp.elevation_number))
        .map(|sp| FrameCell {
            start_secs: sp.collection_start_secs,
            end_secs: sp.collection_end_secs,
            elevation_number: sp.elevation_number,
            elevation_angle: sp.elevation_angle,
            state: FrameCellState::Projected,
            chunks: None,
            is_active: false,
            is_prev_active: false,
        })
        .collect();
    ScanContainer {
        start_secs: ghost.volume_start,
        end_secs: ghost.volume_end,
        key_secs: ghost.volume_start,
        vcp: ghost.vcp_number,
        cells,
        sweep_spans,
        is_available: false,
        is_live: false,
    }
}

/// The displayed right edge of the scan at `scans[i]`.
///
/// Prefers the **authoritative** archive extent: when the scan matches a known
/// archive boundary (by `key_timestamp`, within [`BOUNDARY_MATCH_TOLERANCE_SECS`])
/// that boundary's `end` is the real start time of the *next* volume, so the
/// block ends exactly there. This caps sparse/incomplete scans against the
/// following volume even when that volume is only ghosted (not downloaded) —
/// the VCP-projected extent alone can overrun several ghosted scans.
///
/// With no matching boundary (e.g. no archive listing fetched, as during pure
/// live streaming) it falls back to the VCP projection
/// ([`Scan::display_end_time`]), capped at the next *downloaded* scan's start so
/// sparse blocks still can't overrun a neighbor we do have. Never truncates
/// real collected data: the result is floored at the scan's own `end_time`.
fn clamped_display_end(scans: &[Scan], shadows: &[ScanBoundary], i: usize) -> f64 {
    let scan = &scans[i];
    if let Some(b) = shadows.iter().find(|b| {
        (b.start as f64 - scan.key_timestamp).abs() <= BOUNDARY_MATCH_TOLERANCE_SECS as f64
    }) {
        return (b.end as f64).max(scan.end_time);
    }
    let de = scan.display_end_time();
    scans.get(i + 1).map_or(de, |next| de.min(next.start_time))
}

/// Whether `scan` is the in-progress live volume identified by `live_ms`.
fn is_live_scan(scan: &Scan, live_ms: Option<i64>) -> bool {
    match live_ms {
        Some(ms) => scan.key_ms() == ms,
        None => false,
    }
}

/// Find the cached scan whose key matches `key_ms` (exact, rounded millis).
fn scan_with_key_ms(cache: &RadarTimeline, key_ms: i64) -> Option<&Scan> {
    cache.scans.iter().find(|s| s.key_ms() == key_ms)
}

/// Overlay the already-cached sweeps of a volume onto its in-progress
/// position model — **the fix for the disappearing-sweep bug**.
///
/// For every cached sweep whose elevation is not already `Complete` in the
/// live model, mark it `Complete` with the cached observed start/end times.
/// The live model's own freshly-decoded sweeps win (they already carry
/// sub-second radial timing for this session), so cached data only fills the
/// elevations the live session hasn't produced yet. Idempotent and pure.
pub fn merge_cached_into_live(position: &mut ScanProjection, cached: &Scan) {
    for sw in &cached.sweeps {
        if let Some(p) = position
            .sweeps
            .iter_mut()
            .find(|p| p.elevation_number == sw.elevation_number)
        {
            if p.status != SweepProjectionStatus::CollectedByUs {
                p.status = SweepProjectionStatus::CollectedByUs;
                p.timing = SweepTimingProvenance::Observed;
                p.collection_start_secs = sw.start_time;
                p.collection_end_secs = sw.end_time;
                p.chunks.clear();
            }
        }
    }
}

/// Convenience: the key-millis a [`ScanKey`] resolves to (mirrors how the
/// adapter and renderers round `key_timestamp`).
#[allow(dead_code)]
pub fn scan_key_ms(key: &ScanKey) -> i64 {
    key.scan_start.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexrad::projection::{
        ProjectionScanRole, SweepAvailability, SweepProjection, SweepProjectionStatus,
        SweepTimingProvenance,
    };
    use crate::state::radar_data::Sweep;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep_pos(elev: u8, status: SweepProjectionStatus) -> SweepProjection {
        SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status,
            timing: SweepTimingProvenance::Estimated,
            collection_start_secs: 0.0,
            collection_end_secs: 0.0,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 0.0,
            chunks: Vec::new(),
        }
    }

    fn live_model(sweeps: Vec<SweepProjection>) -> ScanProjection {
        ScanProjection {
            vcp_number: 212,
            vcp_pattern: None,
            roster: crate::state::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: 1_700_000_000.0,
            volume_end: 1_700_000_300.0,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
        }
    }

    fn cached_sweep(elev: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: 0.5 * elev as f32,
            elevation_number: elev,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: Vec::new(),
        }
    }

    fn cached_scan(key_secs: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_secs,
            end_time: key_secs + 300.0,
            key_timestamp: key_secs,
            vcp: 212,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// The bug: a resumed volume's cached cuts must survive while streaming.
    #[wasm_bindgen_test]
    fn merge_keeps_cached_sweeps_while_streaming() {
        // Live session has only seen elev 2 (in progress); elevs 1 & 3 are
        // future. The volume already has elevs 1 & 3 cached from a prefetch.
        let mut pos = live_model(vec![
            sweep_pos(1, SweepProjectionStatus::FutureExpected),
            sweep_pos(2, SweepProjectionStatus::InProgress),
            sweep_pos(3, SweepProjectionStatus::FutureExpected),
        ]);
        let scan = cached_scan(
            1_700_000_000.0,
            vec![
                cached_sweep(1, 1_700_000_001.0, 1_700_000_030.0),
                cached_sweep(3, 1_700_000_060.0, 1_700_000_090.0),
            ],
        );

        merge_cached_into_live(&mut pos, &scan);

        // Cached cuts now render as Cached with their observed times.
        let s1 = &pos.sweeps[0];
        assert_eq!(s1.availability(), SweepAvailability::Cached);
        assert_eq!(s1.timing, SweepTimingProvenance::Observed);
        assert_eq!(s1.collection_start_secs, 1_700_000_001.0);
        assert_eq!(s1.collection_end_secs, 1_700_000_030.0);

        // The in-progress cut is untouched (session data wins).
        assert_eq!(pos.sweeps[1].availability(), SweepAvailability::Collecting);

        let s3 = &pos.sweeps[2];
        assert_eq!(s3.availability(), SweepAvailability::Cached);
        assert_eq!(s3.collection_start_secs, 1_700_000_060.0);

        assert_eq!(pos.completed_count(), 2);
    }

    /// Session-decoded sweeps are authoritative; the merge never clobbers a
    /// cut the live session already completed.
    #[wasm_bindgen_test]
    fn merge_does_not_clobber_session_complete() {
        let mut pos = live_model(vec![{
            let mut p = sweep_pos(1, SweepProjectionStatus::CollectedByUs);
            p.timing = SweepTimingProvenance::Observed;
            p.collection_start_secs = 999.0;
            p.collection_end_secs = 1099.0;
            p
        }]);
        let scan = cached_scan(0.0, vec![cached_sweep(1, 1.0, 2.0)]);
        merge_cached_into_live(&mut pos, &scan);
        // Session times preserved, not overwritten by the cached (1.0, 2.0).
        assert_eq!(pos.sweeps[0].collection_start_secs, 999.0);
        assert_eq!(pos.sweeps[0].collection_end_secs, 1099.0);
    }

    /// `is_covered_by_cached` matches the old 60s tolerance band.
    #[wasm_bindgen_test]
    fn covered_by_cached_uses_60s_band() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1_700_000_000.0);

        assert!(view.is_covered_by_cached(1_700_000_000));
        assert!(view.is_covered_by_cached(1_700_000_059));
        assert!(view.is_covered_by_cached(1_700_000_000 - 60));
        assert!(!view.is_covered_by_cached(1_700_000_061));
        assert!(!view.is_covered_by_cached(1_700_000_000 - 61));
    }

    /// Without a live anchor the cached scan stays on the settled track.
    #[wasm_bindgen_test]
    fn settled_includes_scan_when_not_live() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1_700_000_000.0);
        let n = view
            .settled_scans_in_range(1_699_999_000.0, 1_700_001_000.0)
            .count();
        assert_eq!(n, 1);
        assert!(view.live_volume().is_none());
    }

    /// A scan matching a known archive boundary ends exactly at that
    /// boundary's `end` (the next volume's real start) — even when the scan is
    /// sparse and its VCP projection would overrun the following ghosted scan.
    #[wasm_bindgen_test]
    fn visual_extent_uses_authoritative_archive_boundary() {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};

        // Sparse scan at key 1000: real data [1000, 1010], VCP projects 120s
        // (→ 1120), but the archive says the next volume starts at 1060.
        let mut sparse = cached_scan(1000.0, Vec::new());
        sparse.start_time = 1000.0;
        sparse.end_time = 1010.0;
        sparse.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        let cache = RadarTimeline {
            scans: vec![sparse],
        };
        // Archive listing knows scans at 1000 and 1060 (only 1000 downloaded).
        let shadows = vec![
            ScanBoundary {
                start: 1000,
                end: 1060,
            },
            ScanBoundary {
                start: 1060,
                end: 1120,
            },
        ];
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1000.0);

        let pairs: Vec<_> = view.visual_scans_in_range(0.0, 5000.0).collect();
        assert_eq!(pairs.len(), 1);
        // Clamped to the archive boundary end (1060), not the VCP projection (1120).
        assert_eq!(pairs[0].1, 1060.0);
    }

    /// With no archive listing, the projection is capped at the next
    /// downloaded scan's start, and culling uses that clamped extent.
    #[wasm_bindgen_test]
    fn visual_extent_falls_back_to_next_downloaded_scan() {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};

        let mut sparse = cached_scan(1000.0, Vec::new());
        sparse.start_time = 1000.0;
        sparse.end_time = 1010.0;
        sparse.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        let mut next = cached_scan(1060.0, Vec::new());
        next.start_time = 1060.0;
        next.end_time = 1100.0;
        next.vcp_pattern = None;
        let cache = RadarTimeline {
            scans: vec![sparse, next],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1000.0);

        let pairs: Vec<_> = view.visual_scans_in_range(0.0, 5000.0).collect();
        assert_eq!(pairs.len(), 2);
        // Sparse scan's 1120 projection capped at next downloaded start (1060).
        assert_eq!(pairs[0].1, 1060.0);
        // Last scan, no VCP: display end == end_time.
        assert_eq!(pairs[1].1, 1100.0);

        // Culling uses the clamped extent: a window past the clamp drops it.
        assert_eq!(view.visual_scans_in_range(1065.0, 5000.0).count(), 1);
    }

    // ── Frame-cell join (state in → state out) ───────────────────────────

    /// A cached sweep that lists `products`.
    fn cached_sweep_products(elev: u8, start: f64, end: f64, products: &[&str]) -> Sweep {
        Sweep {
            cached_products: products.iter().map(|p| p.to_string()).collect(),
            ..cached_sweep(elev, start, end)
        }
    }

    /// Join inputs with everything empty: product "reflectivity", no tilt
    /// filter, no download/failure overlap, no active ring.
    fn empty_join<'a>(product: &'a str, tilt: Option<u8>) -> FrameJoinInputs<'a> {
        FrameJoinInputs {
            queued: &[],
            in_flight: &[],
            failed: &[],
            product,
            tilt,
            active: None,
            prev_active: None,
        }
    }

    /// A cached sweep with the selected product → exactly one Cached cell.
    #[wasm_bindgen_test]
    fn join_cached_sweep_is_cached_cell() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(
                1_000.0,
                vec![cached_sweep_products(
                    1,
                    1_000.0,
                    1_030.0,
                    &["reflectivity"],
                )],
            )],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let containers =
            view.frame_containers_in_range(0.0, 5_000.0, empty_join("reflectivity", Some(1)));
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].cells.len(), 1);
        assert_eq!(containers[0].cells[0].state, FrameCellState::Cached);
        assert_eq!(containers[0].cells[0].elevation_number, 1);
        assert!(!containers[0].is_available);
    }

    /// SAILS-style multiple matching cuts in one scan → multiple cells in one
    /// container.
    #[wasm_bindgen_test]
    fn join_sails_revisit_yields_multiple_cells() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(
                1_000.0,
                vec![
                    cached_sweep_products(1, 1_000.0, 1_030.0, &["reflectivity"]),
                    cached_sweep_products(2, 1_030.0, 1_060.0, &["reflectivity"]),
                    // SAILS revisit of tilt 1 mid-volume.
                    cached_sweep_products(1, 1_120.0, 1_150.0, &["reflectivity"]),
                ],
            )],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let containers =
            view.frame_containers_in_range(0.0, 5_000.0, empty_join("reflectivity", Some(1)));
        assert_eq!(containers.len(), 1);
        // Two tilt-1 cells, not the tilt-2 sweep.
        assert_eq!(containers[0].cells.len(), 2);
        assert!(containers[0]
            .cells
            .iter()
            .all(|c| c.elevation_number == 1 && c.state == FrameCellState::Cached));
        // The full structure (all 3 sweeps) is in the sub-texture.
        assert_eq!(containers[0].sweep_spans.len(), 3);
    }

    /// A shadow boundary with no cache → one Available container/cell; the
    /// selected tilt is assumed to exist.
    #[wasm_bindgen_test]
    fn join_shadow_is_available() {
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: 1_000,
            end: 1_300,
        }];
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let containers =
            view.frame_containers_in_range(0.0, 5_000.0, empty_join("reflectivity", Some(1)));
        assert_eq!(containers.len(), 1);
        assert!(containers[0].is_available);
        assert_eq!(containers[0].cells.len(), 1);
        assert_eq!(containers[0].cells[0].state, FrameCellState::Available);
    }

    /// Download-progress ranges upgrade a shadow cell: in-flight wins over
    /// queued wins over failed.
    #[wasm_bindgen_test]
    fn join_shadow_state_precedence() {
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: 1_000,
            end: 1_300,
        }];
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);

        let queued = [(1_010_i64, 1_300_i64)];
        let in_flight = [(990_i64, 1_300_i64)];
        let failed = [1_005_i64];

        // Queued only.
        let mut j = empty_join("reflectivity", Some(1));
        j.queued = &queued;
        let c = view.frame_containers_in_range(0.0, 5_000.0, j);
        assert_eq!(c[0].cells[0].state, FrameCellState::Queued);

        // Failed only.
        let mut j = empty_join("reflectivity", Some(1));
        j.failed = &failed;
        let c = view.frame_containers_in_range(0.0, 5_000.0, j);
        assert_eq!(c[0].cells[0].state, FrameCellState::Failed);

        // In-flight beats queued + failed.
        let mut j = empty_join("reflectivity", Some(1));
        j.in_flight = &in_flight;
        j.queued = &queued;
        j.failed = &failed;
        let c = view.frame_containers_in_range(0.0, 5_000.0, j);
        assert_eq!(c[0].cells[0].state, FrameCellState::InFlight);
    }

    /// A cached scan whose matching tilt lacks the selected product reads
    /// Available (the volume is on device, but not this product).
    #[wasm_bindgen_test]
    fn join_cached_without_product_is_available() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(
                1_000.0,
                vec![cached_sweep_products(1, 1_000.0, 1_030.0, &["velocity"])],
            )],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let c = view.frame_containers_in_range(0.0, 5_000.0, empty_join("reflectivity", Some(1)));
        assert_eq!(c[0].cells[0].state, FrameCellState::Available);
        // Same scan resolved for velocity → Cached.
        let c = view.frame_containers_in_range(0.0, 5_000.0, empty_join("velocity", Some(1)));
        assert_eq!(c[0].cells[0].state, FrameCellState::Cached);
    }

    /// A cached scan whose completeness is `Missing` (indexed, nothing
    /// downloaded, no sweeps) renders as an Available container with one
    /// assumed Available cell for the selected tilt — not a solid empty box.
    #[wasm_bindgen_test]
    fn join_missing_scan_is_available() {
        let mut missing = cached_scan(1_000.0, Vec::new());
        missing.completeness = Some(crate::data::ScanCompleteness::Missing);
        let cache = RadarTimeline {
            scans: vec![missing],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let c = view.frame_containers_in_range(0.0, 5_000.0, empty_join("reflectivity", Some(1)));
        assert_eq!(c.len(), 1);
        assert!(c[0].is_available);
        assert_eq!(c[0].cells.len(), 1);
        assert_eq!(c[0].cells[0].state, FrameCellState::Available);
        assert_eq!(c[0].cells[0].elevation_number, 1);
    }

    /// The live in-progress volume becomes one container: collecting cut →
    /// InFlight with chunk segmentation; future matching cut → Projected;
    /// cached cut → Cached. Non-matching sweeps appear only in `sweep_spans`.
    #[wasm_bindgen_test]
    fn join_live_volume_container() {
        let mut s_inprog = sweep_pos(1, SweepProjectionStatus::InProgress);
        s_inprog.collection_start_secs = 1_700_000_000.0;
        s_inprog.collection_end_secs = 1_700_000_030.0;
        s_inprog.chunks_in_sweep = 3;
        s_inprog.chunks_received = 1;
        let mut s_cached = sweep_pos(1, SweepProjectionStatus::CollectedByUs);
        s_cached.collection_start_secs = 1_700_000_120.0;
        s_cached.collection_end_secs = 1_700_000_150.0;
        let mut s_future = sweep_pos(1, SweepProjectionStatus::FutureExpected);
        s_future.collection_start_secs = 1_700_000_240.0;
        s_future.collection_end_secs = 1_700_000_270.0;
        // A non-matching tilt-2 sweep (sub-texture only).
        let mut s_other = sweep_pos(2, SweepProjectionStatus::CollectedByUs);
        s_other.collection_start_secs = 1_700_000_060.0;
        s_other.collection_end_secs = 1_700_000_090.0;

        let mut pos = live_model(vec![s_inprog, s_other, s_cached, s_future]);
        pos.in_progress_radials = Some(42);

        let live = crate::state::LiveModeState::with_dummy_streaming(
            crate::state::LivePhase::Streaming,
            1_700_000_010.0,
        );
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(
            &cache,
            &shadows,
            Some(&live),
            Some(&pos),
            Some(1),
            1_700_000_010.0,
        );
        let c = view.frame_containers_in_range(
            1_699_999_000.0,
            1_700_001_000.0,
            empty_join("reflectivity", Some(1)),
        );
        // One live container.
        assert_eq!(c.len(), 1);
        let lc = &c[0];
        assert!(lc.is_live);
        // Three tilt-1 cells (in-progress, cached, future); tilt-2 excluded.
        assert_eq!(lc.cells.len(), 3);
        let inflight = lc
            .cells
            .iter()
            .find(|c| c.state == FrameCellState::InFlight)
            .unwrap();
        let chunks = inflight.chunks.as_ref().unwrap();
        assert_eq!(chunks.chunks_expected, 3);
        assert_eq!(chunks.chunks_received, 1);
        assert_eq!(chunks.partial_radials, 42);
        assert!(lc.cells.iter().any(|c| c.state == FrameCellState::Cached));
        assert!(lc
            .cells
            .iter()
            .any(|c| c.state == FrameCellState::Projected));
        // All four sweeps contribute to the sub-texture.
        assert_eq!(lc.sweep_spans.len(), 4);
    }

    /// The active-ring identity flags exactly the on-GPU cell.
    #[wasm_bindgen_test]
    fn join_active_ring_flags_one_cell() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(
                1_000.0,
                vec![
                    cached_sweep_products(1, 1_000.0, 1_030.0, &["reflectivity"]),
                    cached_sweep_products(1, 1_120.0, 1_150.0, &["reflectivity"]),
                ],
            )],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, Some(1), 2_000.0);
        let mut j = empty_join("reflectivity", Some(1));
        // The scan key is 1_000; the on-GPU cell is tilt 1.
        j.active = Some((1_000.0, 1));
        let c = view.frame_containers_in_range(0.0, 5_000.0, j);
        let active_count = c[0].cells.iter().filter(|c| c.is_active).count();
        assert_eq!(active_count, 2); // both tilt-1 cells share the scan key + elev
    }
}
