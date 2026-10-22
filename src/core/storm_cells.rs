//! Storm cell detection: which cut it runs on, when it runs, what result is
//! kept, and what the Layers panel shows for it.
//!
//! The detector itself (the `crate::storm_cells` leaf module) is a pure
//! function of one sweep. It runs in a decode worker (the `detect` message),
//! on the volume's lowest reflectivity cut, read from IndexedDB or, while a
//! live stream is still filling that cut, from the live accumulator. This
//! module decides what to ask for ([`decide`]), describes the request as an
//! effect ([`DetectRequest`]) and takes the reply back ([`StormCells::accept`]).
//!
//! - **One volume, one set of cells.** The input is the volume's lowest
//!   reflectivity cut ([`lowest_reflectivity_cut`]: elevation 1 of a split
//!   cut, the surveillance half), whatever tilt and product are on screen,
//!   so the overlay does not change as the tilt ladder moves, the Doppler
//!   cut's range gap cannot split bands, and the cells stay over velocity,
//!   correlation coefficient and the rest (a couplet inside a cell). When that cut was never ingested (a fixed-tilt fetch
//!   brings only one cut) the displayed sweep is used instead if it is
//!   reflectivity, and the key says so ([`StormCellKey::fallback`]).
//! - **Live.** While the stream fills the cut, results are provisional and
//!   keyed by the radials received; once the cut is stored, the stored sweep
//!   gives the final result.
//! - **Cache.** Results, empty ones included, are kept per key
//!   ([`StormCellKey`]: volume, cut, input, threshold) in a small LRU, so
//!   nothing re-runs on a repaint, a drag, a 3D frame or a tilt change, and
//!   flipping back to a volume shows its cells at once.
//! - **Volume.** Once the displayed volume's cells are shown from the stored
//!   lowest cut, a second request samples the volume's other reflectivity
//!   tilts over the cells' footprints ([`volume_cuts`] picks one cut per
//!   tilt) for each cell's column: echo top, composite peak and its height.
//!   It goes after the cells, so their latency does not change, and is
//!   asked again when more of the volume's cuts are on device. The columns
//!   are provisional ([`crate::storm_cells::VolumeStructure::complete`])
//!   until every planned tilt is sampled. Only the displayed volume gets
//!   one; history volumes (for tracking) do not.
//! - **Staleness.** One request is in flight at a time and replies carry its
//!   id; a reply whose request is no longer in flight (a site change reset the
//!   state, or the request timed out) is dropped. A cached result is shown
//!   only for the exact key that is wanted, so a reply for a key the view has
//!   moved past is never drawn, and a newer provisional or the stored result
//!   evicts older provisional ones for the same cut.

use crate::core::{DisplayedSweep, RadarProduct, Scan, StormCellsResult, Sweep};
use crate::data::{ExtractedVcp, ScanKey};
use crate::geo::ViewMode;
use crate::storm_cells::DetectionParams;

/// Lowest threshold the Layers slider offers, in dBZ.
pub(crate) const THRESHOLD_MIN_DBZ: f32 = 20.0;
/// Highest threshold the Layers slider offers, in dBZ.
pub(crate) const THRESHOLD_MAX_DBZ: f32 = 60.0;
/// Threshold used when a stored value is unusable.
pub(crate) const THRESHOLD_DEFAULT_DBZ: f32 = 35.0;

/// Clamp a threshold (e.g. one read back from stored preferences) to the
/// slider's range. NaN falls back to the default.
pub(crate) fn clamp_threshold(dbz: f32) -> f32 {
    if dbz.is_nan() {
        THRESHOLD_DEFAULT_DBZ
    } else {
        dbz.clamp(THRESHOLD_MIN_DBZ, THRESHOLD_MAX_DBZ)
    }
}

/// Results kept in the cache (per volume, cut and threshold). A volume is
/// small (tens of cells with outlines), so this holds a long scrub's worth.
pub(crate) const CACHE_CAPACITY: usize = 48;

/// A request with no reply after this long is given up, so a lost reply
/// (a worker that died) cannot block detection for good.
pub(crate) const REQUEST_TIMEOUT_MS: f64 = 15_000.0;

/// Worker-string name of the reflectivity product.
const REFLECTIVITY: &str = "reflectivity";

/// Cuts within this many degrees of the lowest angle count as the same tilt
/// (a split cut's two halves, SAILS/MRLE rescans). Measured angles of one
/// commanded tilt spread by a few hundredths; distinct VCP tilts are at least
/// 0.4° apart.
const SAME_TILT_DEG: f32 = 0.2;

/// Where the worker reads the cut from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CutInput {
    /// The stored sweep blob in IndexedDB: a complete cut.
    Stored,
    /// The live accumulator, holding `radials` radials of the cut so far.
    Live { radials: u32 },
}

/// What a detection result is computed for. A result is reused only for an
/// equal key, so this versions results: a new live chunk, another volume or
/// another threshold is a different key.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct StormCellKey {
    /// The volume.
    pub scan_key: ScanKey,
    /// The cut detected on.
    pub elevation_number: u8,
    pub input: CutInput,
    /// The volume's lowest reflectivity cut is not available, so this is the
    /// displayed sweep instead.
    pub fallback: bool,
    pub threshold_dbz: f32,
}

impl StormCellKey {
    /// Same volume, cut, role and threshold; the input may differ.
    pub(crate) fn same_cut(&self, other: &Self) -> bool {
        self.scan_key == other.scan_key
            && self.elevation_number == other.elevation_number
            && self.fallback == other.fallback
            && self.threshold_dbz == other.threshold_dbz
    }
}

/// Worker-side timing and grid size of one detection run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct DetectTiming {
    /// Reading the cut: the IndexedDB blob, or the accumulator's radials.
    pub fetch_ms: f64,
    /// `storm_cells::detect_cells` alone.
    pub detect_ms: f64,
    /// The whole worker call.
    pub total_ms: f64,
    pub azimuth_count: u32,
    pub gate_count: u32,
}

/// The cuts a volume request samples: one reflectivity cut per distinct
/// tilt, lowest angle first, so the first is the lowest reflectivity cut.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeCuts {
    /// Elevation numbers, lowest angle first.
    pub cuts: Vec<u8>,
    /// Every tilt of the volume's plan has a cut here (without a plan, the
    /// stored tilts are taken as the whole volume).
    pub complete: bool,
}

/// One stored reflectivity cut per distinct tilt of the volume, and whether
/// that covers the plan.
///
/// Tilts are angles within [`SAME_TILT_DEG`] of each other (the VCP's
/// commanded angle when the plan is known, else the measured one), so a
/// split cut's two halves and SAILS/MRLE rescans count once; of a tilt's
/// stored cuts the first collected (the lowest elevation number) is taken,
/// the surveillance half of a split cut, as for [`lowest_reflectivity_cut`].
pub(crate) fn volume_cuts(vcp: Option<&ExtractedVcp>, sweeps: &[Sweep]) -> VolumeCuts {
    let plan = vcp.filter(|v| !v.elevations.is_empty());
    let angle_of = |s: &Sweep| -> f32 {
        plan.and_then(|v| v.elevations.get(s.elevation_number as usize - 1))
            .map_or(s.elevation, |e| e.angle)
    };
    let mut stored: Vec<(f32, u8)> = sweeps
        .iter()
        .filter(|s| has_reflectivity(s) && s.elevation_number > 0)
        .map(|s| (angle_of(s), s.elevation_number))
        .filter(|(a, _)| a.is_finite())
        .collect();
    stored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut planned: Vec<f32> = match plan {
        Some(v) => v.elevations.iter().map(|e| e.angle).collect(),
        None => stored.iter().map(|&(a, _)| a).collect(),
    };
    planned.retain(|a| a.is_finite());
    planned.sort_by(f32::total_cmp);
    // Group the planned angles into tilts: (lowest, highest) of each.
    let mut tilts: Vec<(f32, f32)> = Vec::new();
    for a in planned {
        match tilts.last_mut() {
            Some(t) if a - t.1 <= SAME_TILT_DEG => t.1 = a,
            _ => tilts.push((a, a)),
        }
    }
    let mut cuts = Vec::new();
    let mut complete = true;
    for (lo, hi) in tilts {
        let cut = stored
            .iter()
            .filter(|(a, _)| *a >= lo - SAME_TILT_DEG && *a <= hi + SAME_TILT_DEG)
            .map(|&(_, n)| n)
            .min();
        match cut {
            Some(n) if !cuts.contains(&n) => cuts.push(n),
            Some(_) => {}
            None => complete = false,
        }
    }
    VolumeCuts { cuts, complete }
}

/// Effect: ask a decode worker to run detection. The shell posts it as a
/// `detect` message and feeds the reply to [`StormCells::accept`] (or a
/// failure to [`StormCells::fail`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetectRequest {
    /// Correlates the reply with this request.
    pub request_id: u64,
    pub key: StormCellKey,
    pub params: DetectionParams,
    pub radar_lat: f64,
    pub radar_lon: f64,
    /// For a volume request, the cuts to sample, the detected cut first
    /// ([`VolumeCuts::cuts`]); empty for cells alone.
    pub volume_cuts: Vec<u8>,
}

/// A worker's answer to a [`DetectRequest`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetectReply {
    pub request_id: u64,
    /// Cells and groups; `result.source` names the cut (sweep id, product,
    /// elevation angle) and the parameters.
    pub result: StormCellsResult,
    pub timing: DetectTiming,
    /// The cut was complete when it was read: stored, or a live cut that the
    /// stream finished before the request ran.
    pub complete: bool,
}

/// What became of a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplyDisposition {
    /// Cached under its key; [`decide`] shows it while that key is wanted.
    Cached,
    /// Not the request in flight (superseded by a reset or a timeout).
    Dropped,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    key: StormCellKey,
    result: StormCellsResult,
    /// `None` for a failed run (cached as empty so it is not retried).
    timing: Option<DetectTiming>,
    complete: bool,
    /// The cuts the volume part was last asked for (answered, failed or
    /// not attachable), so it is not asked again until more are on device.
    volume_for: Option<VolumeCuts>,
}

#[derive(Clone, Debug)]
struct InFlight {
    request_id: u64,
    key: StormCellKey,
    sent_ms: f64,
    /// A volume request for these cuts.
    volume: Option<VolumeCuts>,
}

/// The shown detection result, the cache behind it, and the request in
/// flight.
#[derive(Clone, Debug, Default)]
pub(crate) struct StormCells {
    /// The cells and groups on screen. Empty is a valid result.
    pub result: StormCellsResult,
    /// The key `result` belongs to; `None` when nothing is shown.
    pub computed_for: Option<StormCellKey>,
    /// Worker timing of the shown result (`None` for a failed run).
    pub timing: Option<DetectTiming>,
    /// The shown result was computed on a complete cut.
    complete: bool,
    /// The cache entry's `volume_for` when the result was shown, to tell
    /// when a newer volume part has landed.
    shown_volume_for: Option<VolumeCuts>,
    /// Least recently used first.
    cache: Vec<CacheEntry>,
    in_flight: Option<InFlight>,
    next_request_id: u64,
}

impl StormCells {
    /// Stop showing a result. The cache and any request in flight stay, so
    /// turning the layer back on (or returning to the volume) is instant.
    pub(crate) fn clear(&mut self) {
        self.result.clear();
        self.computed_for = None;
        self.timing = None;
        self.complete = false;
        self.shown_volume_for = None;
    }

    /// Forget everything: the shown result, the cache, and the request in
    /// flight, whose reply will then be dropped. For a site change.
    pub(crate) fn reset(&mut self) {
        let next_request_id = self.next_request_id;
        *self = Self {
            next_request_id,
            ..Self::default()
        };
    }

    /// The shown cells come from a live cut the stream has not finished.
    pub(crate) fn is_provisional(&self) -> bool {
        self.computed_for
            .as_ref()
            .is_some_and(|k| matches!(k.input, CutInput::Live { .. }))
            && !self.complete
    }

    fn shows_nothing(&self) -> bool {
        self.result.is_empty() && self.computed_for.is_none()
    }

    fn cached(&self, key: &StormCellKey) -> Option<&CacheEntry> {
        self.cache.iter().find(|e| &e.key == key)
    }

    /// The cached result for `key`, shown or not (a failed run is an empty
    /// result). Tracking reads the preceding volumes' cells from here.
    pub(crate) fn cached_result(&self, key: &StormCellKey) -> Option<&StormCellsResult> {
        self.cached(key).map(|e| &e.result)
    }

    /// Execute an action's state change. Returns the worker request to post
    /// for [`StormCellAction::Request`].
    pub(crate) fn apply(
        &mut self,
        action: StormCellAction,
        env: &StormCellEnv<'_>,
    ) -> Option<DetectRequest> {
        match action {
            StormCellAction::Keep => None,
            StormCellAction::Clear => {
                self.clear();
                None
            }
            StormCellAction::Show(key) => {
                if let Some(i) = self.cache.iter().position(|e| e.key == key) {
                    // Most recently used goes last.
                    let entry = self.cache.remove(i);
                    self.result = entry.result.clone();
                    self.timing = entry.timing;
                    self.complete = entry.complete;
                    self.shown_volume_for = entry.volume_for.clone();
                    self.computed_for = Some(key);
                    self.cache.push(entry);
                }
                None
            }
            StormCellAction::Request(key) => Some(self.request(key, None, env)),
            StormCellAction::RequestVolume(key, cuts) => Some(self.request(key, Some(cuts), env)),
        }
    }

    fn request(
        &mut self,
        key: StormCellKey,
        volume: Option<VolumeCuts>,
        env: &StormCellEnv<'_>,
    ) -> DetectRequest {
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        let volume_cuts = volume.as_ref().map_or_else(Vec::new, |v| v.cuts.clone());
        self.in_flight = Some(InFlight {
            request_id,
            key: key.clone(),
            sent_ms: env.now_ms,
            volume,
        });
        DetectRequest {
            request_id,
            params: DetectionParams {
                threshold_dbz: key.threshold_dbz,
                ..DetectionParams::default()
            },
            key,
            radar_lat: env.radar_lat,
            radar_lon: env.radar_lon,
            volume_cuts,
        }
    }

    /// Take a worker reply. It is cached only if it answers the request in
    /// flight. It is not shown here: the next [`decide`] shows it if
    /// its key is still the one wanted.
    ///
    /// A volume reply adds its columns to the cached result for its key
    /// (marked complete or not by the cuts asked for). The worker detects
    /// the stored cut again to sample over the footprints, so its cells are
    /// the cached ones; should they differ, the columns are not attached
    /// (they would describe other cells), and either way the cuts are not
    /// asked for again.
    pub(crate) fn accept(&mut self, reply: DetectReply) -> ReplyDisposition {
        let Some((key, volume)) = self.take_in_flight(reply.request_id) else {
            return ReplyDisposition::Dropped;
        };
        if let Some(cuts) = volume {
            let mut result = reply.result;
            if let Some(v) = result.volume.as_mut() {
                v.complete = cuts.complete;
            }
            match self.cache.iter_mut().find(|e| e.key == key) {
                Some(entry) => {
                    if entry.result.cells == result.cells {
                        entry.result.volume = result.volume;
                    }
                    entry.volume_for = Some(cuts);
                }
                None => {
                    self.insert(key.clone(), result, Some(reply.timing), reply.complete);
                    if let Some(entry) = self.cache.last_mut() {
                        entry.volume_for = Some(cuts);
                    }
                }
            }
            return ReplyDisposition::Cached;
        }
        // A live cut that finished before the worker read it is the stored
        // cut's final answer too.
        let stored_twin = (reply.complete && key.input != CutInput::Stored).then(|| StormCellKey {
            input: CutInput::Stored,
            ..key.clone()
        });
        self.insert(
            key,
            reply.result.clone(),
            Some(reply.timing),
            reply.complete,
        );
        if let Some(stored) = stored_twin {
            self.insert(stored, reply.result, Some(reply.timing), true);
        }
        ReplyDisposition::Cached
    }

    /// The request failed in the worker. Cache an empty result for its key so
    /// the failure is not retried every frame; a different key (the next live
    /// chunk, the cut once stored) asks again.
    /// A failed volume request leaves the cells alone and is not retried
    /// for the same cuts.
    pub(crate) fn fail(&mut self, request_id: u64) -> ReplyDisposition {
        let Some((key, volume)) = self.take_in_flight(request_id) else {
            return ReplyDisposition::Dropped;
        };
        if let Some(cuts) = volume {
            if let Some(entry) = self.cache.iter_mut().find(|e| e.key == key) {
                entry.volume_for = Some(cuts);
            }
            return ReplyDisposition::Cached;
        }
        let result = StormCellsResult {
            source: crate::storm_cells::DetectionSource {
                params: DetectionParams {
                    threshold_dbz: key.threshold_dbz,
                    ..DetectionParams::default()
                },
                ..Default::default()
            },
            ..StormCellsResult::default()
        };
        self.insert(key, result, None, false);
        ReplyDisposition::Cached
    }

    /// Whether `request_id` is the request in flight, i.e. its reply would
    /// be accepted. Routes a reply to the site that sent it.
    pub(crate) fn awaits(&self, request_id: u64) -> bool {
        self.in_flight
            .as_ref()
            .is_some_and(|f| f.request_id == request_id)
    }

    fn take_in_flight(&mut self, request_id: u64) -> Option<(StormCellKey, Option<VolumeCuts>)> {
        match &self.in_flight {
            Some(f) if f.request_id == request_id => {
                self.in_flight.take().map(|f| (f.key, f.volume))
            }
            _ => None,
        }
    }

    fn insert(
        &mut self,
        key: StormCellKey,
        result: StormCellsResult,
        timing: Option<DetectTiming>,
        complete: bool,
    ) {
        // Older provisional results for the cut are dead once a newer one or
        // the stored one exists.
        let is_stored = key.input == CutInput::Stored;
        self.cache.retain(|e| {
            !(e.key == key
                || (e.key.same_cut(&key)
                    && matches!(e.key.input, CutInput::Live { .. })
                    && (is_stored || matches!(key.input, CutInput::Live { .. }))))
        });
        self.cache.push(CacheEntry {
            key,
            result,
            timing,
            complete,
            volume_for: None,
        });
        if self.cache.len() > CACHE_CAPACITY {
            let excess = self.cache.len() - CACHE_CAPACITY;
            self.cache.drain(..excess);
        }
    }

    /// Put a result in the cache (and on screen when `show`), as a reply
    /// followed by a `Show` would.
    #[cfg(test)]
    pub(crate) fn seed(
        &mut self,
        key: StormCellKey,
        result: StormCellsResult,
        complete: bool,
        show: bool,
    ) {
        self.insert(key.clone(), result.clone(), None, complete);
        if show {
            self.result = result;
            self.computed_for = Some(key);
            self.timing = None;
            self.complete = complete;
        }
    }

    /// Developer-mode summary of the shown result, `None` when nothing is
    /// shown.
    pub(crate) fn stats(&self) -> Option<StormCellStats> {
        let key = self.computed_for.as_ref()?;
        Some(StormCellStats {
            cells: self.result.cells.len() as u32,
            groups: self.result.groups.len() as u32,
            elevation_number: key.elevation_number,
            elevation_deg: self.result.source.elevation_deg,
            timing: self.timing,
            provisional: self.is_provisional(),
            fallback: key.fallback,
        })
    }
}

/// The developer-mode readout for the shown result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StormCellStats {
    pub cells: u32,
    pub groups: u32,
    pub elevation_number: u8,
    pub elevation_deg: Option<f32>,
    /// `None` when the worker run failed.
    pub timing: Option<DetectTiming>,
    pub provisional: bool,
    pub fallback: bool,
}

impl StormCellStats {
    /// One line for the activity sheet's details, e.g.
    /// `5 cells, 1 group · cut 1 (0.5°) · 84 ms detect / 97 ms total`.
    pub(crate) fn label(&self) -> String {
        let plural =
            |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut s = format!(
            "{}, {} · cut {}",
            plural(self.cells, "cell", "cells"),
            plural(self.groups, "group", "groups"),
            self.elevation_number,
        );
        if let Some(deg) = self.elevation_deg {
            s.push_str(&format!(" ({deg:.1}°)"));
        }
        match self.timing {
            Some(t) => s.push_str(&format!(
                " · {:.0} ms detect / {:.0} ms total",
                t.detect_ms, t.total_ms
            )),
            None => s.push_str(" · failed"),
        }
        if self.fallback {
            s.push_str(" · displayed tilt");
        }
        if self.provisional {
            s.push_str(" · provisional");
        }
        s
    }
}

/// The cut a live stream is filling, from the worker's chunk
/// acknowledgements.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveCutProgress {
    pub scan_key: ScanKey,
    pub elevation_number: u8,
    /// Radials of the cut in the accumulator.
    pub radials: u32,
}

/// Read-only inputs to [`decide`], shell-assembled each time it asks.
pub(crate) struct StormCellEnv<'a> {
    /// `viz_state.storm_cells_visible`.
    pub visible: bool,
    /// `viz_state.view_mode()`.
    pub view_mode: ViewMode,
    /// the site's `displayed` — what is on the GPU main slot. Its volume is
    /// the volume cells are shown for.
    pub displayed: Option<&'a DisplayedSweep>,
    /// `viz_state.storm_cell_threshold_dbz`.
    pub threshold_dbz: f32,
    /// The timeline's scan for the displayed volume, if it has one.
    pub volume: Option<&'a Scan>,
    /// The VCP of the live volume, when the displayed volume is the one
    /// streaming (the timeline may not list it yet).
    pub live_vcp: Option<&'a ExtractedVcp>,
    /// The cut the live stream is filling, if any.
    pub live_cut: Option<&'a LiveCutProgress>,
    /// Radar site position, passed to the worker for cell positions.
    pub radar_lat: f64,
    pub radar_lon: f64,
    /// Frame time, for the request timeout.
    pub now_ms: f64,
    /// Cuts of the volumes before the displayed one that tracking needs
    /// detected (oldest first, see `core::storm_tracks::history_keys`);
    /// empty when tracks are off.
    pub history: &'a [StormCellKey],
}

/// What the shell does with the storm cell result.
#[derive(Debug, PartialEq)]
pub(crate) enum StormCellAction {
    /// Leave everything as it is.
    Keep,
    /// Stop showing the result ([`StormCells::clear`]).
    Clear,
    /// Show the cached result for this key.
    Show(StormCellKey),
    /// Ask a worker to detect for this key ([`StormCells::apply`] returns
    /// the request).
    Request(StormCellKey),
    /// Ask a worker for the columns of the shown key's cells through these
    /// cuts of its volume.
    RequestVolume(StormCellKey, VolumeCuts),
}

/// The elevation number of a volume's lowest reflectivity cut: the lowest
/// angle, and among the cuts at that angle (a split cut's surveillance and
/// Doppler halves, SAILS/MRLE rescans) the first collected, which is the
/// surveillance half with the full range.
///
/// The VCP plan decides when known, so a live volume knows its cut before
/// the cut is stored and a fixed-tilt fetch knows the cut it lacks.
/// Without a plan, the stored reflectivity sweeps decide by measured angle.
pub(crate) fn lowest_reflectivity_cut(vcp: Option<&ExtractedVcp>, sweeps: &[Sweep]) -> Option<u8> {
    let cuts: Vec<(u8, f32)> = match vcp.filter(|v| !v.elevations.is_empty()) {
        Some(vcp) => vcp
            .elevations
            .iter()
            .enumerate()
            .map(|(i, e)| ((i + 1) as u8, e.angle))
            .collect(),
        None => sweeps
            .iter()
            .filter(|s| has_reflectivity(s))
            .map(|s| (s.elevation_number, s.elevation))
            .collect(),
    };
    let lowest = cuts
        .iter()
        .map(|&(_, angle)| angle)
        .filter(|a| a.is_finite())
        .fold(f32::INFINITY, f32::min);
    cuts.iter()
        .filter(|&&(_, angle)| angle - lowest <= SAME_TILT_DEG)
        .map(|&(n, _)| n)
        .min()
}

pub(crate) fn has_reflectivity(sweep: &Sweep) -> bool {
    sweep.cached_products.iter().any(|p| p == REFLECTIVITY)
}

/// The key for detecting on cut `elevation_number` of the displayed volume,
/// if that cut can be read: stored with reflectivity, or being filled by the
/// live stream.
fn cut_key(
    env: &StormCellEnv<'_>,
    scan_key: &ScanKey,
    elevation_number: u8,
    fallback: bool,
) -> Option<StormCellKey> {
    let stored = env.volume.is_some_and(|scan| {
        scan.sweeps
            .iter()
            .any(|s| s.elevation_number == elevation_number && has_reflectivity(s))
    });
    let input = if stored {
        CutInput::Stored
    } else {
        let live = env.live_cut.filter(|c| {
            &c.scan_key == scan_key && c.elevation_number == elevation_number && c.radials > 0
        })?;
        CutInput::Live {
            radials: live.radials,
        }
    };
    Some(StormCellKey {
        scan_key: scan_key.clone(),
        elevation_number,
        input,
        fallback,
        threshold_dbz: env.threshold_dbz,
    })
}

/// What [`decide`] wants for the displayed volume.
enum Wanted {
    Key(StormCellKey),
    /// The lowest cut's cells are held or on their way (its live stream just
    /// finished it and the timeline has not listed it yet): wait rather
    /// than fall back.
    Hold,
    Nothing,
}

fn wanted(env: &StormCellEnv<'_>, displayed: &DisplayedSweep, state: &StormCells) -> Wanted {
    let scan_key = &displayed.identity.scan_key;
    let vcp = env
        .volume
        .and_then(|s| s.vcp_pattern.as_ref())
        .or(env.live_vcp);
    let sweeps = env.volume.map(|s| s.sweeps.as_slice()).unwrap_or(&[]);
    if let Some(target) = lowest_reflectivity_cut(vcp, sweeps) {
        if let Some(key) = cut_key(env, scan_key, target, false) {
            return Wanted::Key(key);
        }
        let has_target = |k: &StormCellKey| {
            &k.scan_key == scan_key
                && k.elevation_number == target
                && !k.fallback
                && k.threshold_dbz == env.threshold_dbz
        };
        if state.computed_for.as_ref().is_some_and(has_target)
            || state.in_flight.as_ref().is_some_and(|f| has_target(&f.key))
        {
            return Wanted::Hold;
        }
    }
    if displayed.identity.product == REFLECTIVITY {
        if let Some(key) = cut_key(env, scan_key, displayed.identity.elevation_number, true) {
            return Wanted::Key(key);
        }
    }
    Wanted::Nothing
}

/// Decide what to show, and whether a worker has to run.
///
/// - Off or nothing displayed: clear. The product on screen does not matter:
///   cells come from the lowest reflectivity cut whatever is shown.
/// - Globe3D: keep. Cells are not drawn there; a changed volume waits for
///   the return to 2D.
/// - Otherwise want the displayed volume's lowest reflectivity cut (or the
///   displayed reflectivity sweep when that cut is unavailable): keep it if
///   shown, show it if cached, else request it, one request at a time. While
///   a request is in flight the previous result stays.
/// - Once it is shown from the stored cut, request its volume part when
///   higher tilts are on device and the cached result lacks it for exactly
///   those cuts ([`wanted_volume`]); show a volume part once it lands.
/// - Then request the [`StormCellEnv::history`] cuts that are not cached,
///   nearest volume first, still one at a time. The displayed volume always
///   goes first.
pub(crate) fn decide(env: &StormCellEnv<'_>, state: &StormCells) -> StormCellAction {
    let clear = || {
        if state.shows_nothing() {
            StormCellAction::Keep
        } else {
            StormCellAction::Clear
        }
    };
    if !env.visible {
        return clear();
    }
    let Some(displayed) = env.displayed else {
        return clear();
    };
    if env.view_mode == ViewMode::Globe3D {
        return StormCellAction::Keep;
    }
    let key = match wanted(env, displayed, state) {
        Wanted::Key(key) => key,
        Wanted::Hold => return StormCellAction::Keep,
        Wanted::Nothing => {
            // Same volume (a velocity sweep still on the GPU): keep its
            // cells; another volume's cells would be wrong here.
            let same_volume = state
                .computed_for
                .as_ref()
                .is_some_and(|k| k.scan_key == displayed.identity.scan_key);
            return if same_volume {
                StormCellAction::Keep
            } else {
                clear()
            };
        }
    };
    let busy = state
        .in_flight
        .as_ref()
        .is_some_and(|f| env.now_ms - f.sent_ms < REQUEST_TIMEOUT_MS);
    if state.computed_for.as_ref() == Some(&key) {
        let entry = state.cached(&key);
        // A newer volume part landed for the shown cells.
        if let Some(e) = entry.filter(|e| e.volume_for != state.shown_volume_for) {
            return StormCellAction::Show(e.key.clone());
        }
        if let Some(cuts) = wanted_volume(env, &key) {
            if entry.is_some_and(|e| e.volume_for.as_ref() != Some(&cuts)) {
                return if busy {
                    StormCellAction::Keep
                } else {
                    StormCellAction::RequestVolume(key, cuts)
                };
            }
        }
        let missing = env.history.iter().rev().find(|h| state.cached(h).is_none());
        return match missing {
            Some(h) if !busy => StormCellAction::Request(h.clone()),
            _ => StormCellAction::Keep,
        };
    }
    if state.cached(&key).is_some() {
        return StormCellAction::Show(key);
    }
    if busy {
        return StormCellAction::Keep;
    }
    StormCellAction::Request(key)
}

/// The cuts to sample for the columns of `key`'s cells: the displayed
/// volume's [`volume_cuts`], when the cells come from its stored lowest cut
/// and at least one higher tilt is on device.
fn wanted_volume(env: &StormCellEnv<'_>, key: &StormCellKey) -> Option<VolumeCuts> {
    if key.input != CutInput::Stored || key.fallback {
        return None;
    }
    let scan = env.volume?;
    let vcp = scan.vcp_pattern.as_ref().or(env.live_vcp);
    let cuts = volume_cuts(vcp, &scan.sweeps);
    (cuts.cuts.len() >= 2 && cuts.cuts[0] == key.elevation_number).then_some(cuts)
}

/// View-model for the threshold slider under the Storm cells checkbox.
#[derive(Debug, PartialEq)]
pub(crate) struct ThresholdSliderVm {
    /// The slider accepts input.
    pub enabled: bool,
    /// Short note shown beside a disabled slider.
    pub hint: Option<&'static str>,
}

/// The slider works over every product (cells always come from the lowest
/// reflectivity cut); over any other product a hint says that its dBZ are
/// reflectivity's.
pub(crate) fn threshold_slider(product: RadarProduct) -> ThresholdSliderVm {
    ThresholdSliderVm {
        enabled: true,
        hint: (product != RadarProduct::Reflectivity).then_some("Cells from reflectivity"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StormCell, SweepIdentity};
    use crate::data::keys::UnixMillis;
    use crate::data::ExtractedVcpElevation;
    use wasm_bindgen_test::wasm_bindgen_test;

    const T0_MS: i64 = 1_716_883_200_000;

    fn scan_key(i: i64) -> ScanKey {
        ScanKey::new("KFWS", UnixMillis(T0_MS + i * 300_000))
    }

    fn displayed(vol: i64, elev: u8, product: &str) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(scan_key(vol), elev, product),
            start_time: 1_716_883_200.0,
            end_time: 1_716_883_220.0,
            elevation_deg: 0.5,
        }
    }

    fn vcp(angles: &[f32]) -> ExtractedVcp {
        ExtractedVcp {
            number: 215,
            elevations: angles
                .iter()
                .map(|&angle| ExtractedVcpElevation {
                    angle,
                    waveform: "CS".into(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: false,
                    azimuth_rate: None,
                })
                .collect(),
        }
    }

    fn stored(elev: u8, angle: f32, products: &[&str]) -> Sweep {
        Sweep {
            start_time: 0.0,
            end_time: 0.0,
            elevation: angle,
            elevation_number: elev,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: products.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// A split-cut volume (0.5 CS, 0.5 CD, 0.9 CS, 0.9 CD, 1.3) with the
    /// given cuts stored as reflectivity + velocity.
    fn volume(vol: i64, stored_cuts: &[u8]) -> Scan {
        let angles = [0.5, 0.5, 0.9, 0.9, 1.3];
        Scan {
            start_time: 0.0,
            end_time: 0.0,
            key_timestamp: scan_key(vol).scan_start.as_secs_f64(),
            vcp: 215,
            vcp_pattern: Some(vcp(&angles)),
            sweeps: stored_cuts
                .iter()
                .map(|&e| stored(e, angles[e as usize - 1], &["reflectivity", "velocity"]))
                .collect(),
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn env<'a>(
        displayed: Option<&'a DisplayedSweep>,
        volume: Option<&'a Scan>,
    ) -> StormCellEnv<'a> {
        StormCellEnv {
            visible: true,
            view_mode: ViewMode::Flat2D,
            displayed,
            threshold_dbz: 35.0,
            volume,
            live_vcp: None,
            live_cut: None,
            radar_lat: 32.57,
            radar_lon: -97.30,
            now_ms: 1_000.0,
            history: &[],
        }
    }

    fn live<'a>(
        d: &'a DisplayedSweep,
        plan: &'a ExtractedVcp,
        progress: &'a LiveCutProgress,
    ) -> StormCellEnv<'a> {
        StormCellEnv {
            live_vcp: Some(plan),
            live_cut: Some(progress),
            ..env(Some(d), None)
        }
    }

    fn cells(n: usize) -> StormCellsResult {
        let cell = |id| StormCell {
            id,
            group_id: None,
            level_dbz: 35.0,
            peak_dbz: 50.0,
            mean_dbz: 42.0,
            area_km2: 30.0,
            gate_count: 20,
            lat: 32.0,
            lon: -97.0,
            bounds: (31.9, -97.1, 32.1, -96.9),
            bearing_from_radar_deg: 0.0,
            range_from_radar_km: 10.0,
            orientation_deg: 0.0,
            elongation: 1.0,
            outline: Vec::new(),
            core_outline: Vec::new(),
        };
        StormCellsResult {
            cells: (0..n as u32).map(cell).collect(),
            ..StormCellsResult::default()
        }
    }

    fn timing() -> DetectTiming {
        DetectTiming {
            fetch_ms: 5.0,
            detect_ms: 80.0,
            total_ms: 90.0,
            azimuth_count: 720,
            gate_count: 1832,
        }
    }

    fn reply(req: &DetectRequest, result: StormCellsResult, complete: bool) -> DetectReply {
        DetectReply {
            request_id: req.request_id,
            result,
            timing: timing(),
            complete,
        }
    }

    /// One frame of the shell: decide, apply, and return the request posted.
    fn frame(state: &mut StormCells, env: &StormCellEnv<'_>) -> Option<DetectRequest> {
        let action = decide(env, state);
        state.apply(action, env)
    }

    /// A frame that must post a request; returns it.
    fn requested(state: &mut StormCells, env: &StormCellEnv<'_>) -> DetectRequest {
        frame(state, env).expect("expected a detect request")
    }

    // ── Cut selection ─────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn lowest_cut_is_the_surveillance_half_of_a_split_cut() {
        // CS 0.5, CD 0.5, ..., SAILS 0.5 rescan at the end.
        let plan = vcp(&[0.5, 0.5, 0.9, 0.9, 1.3, 0.5, 1.8]);
        assert_eq!(lowest_reflectivity_cut(Some(&plan), &[]), Some(1));
        // A plan whose lowest tilt is not first.
        let plan = vcp(&[0.9, 0.5, 0.5]);
        assert_eq!(lowest_reflectivity_cut(Some(&plan), &[]), Some(2));
    }

    #[wasm_bindgen_test]
    fn without_a_plan_the_stored_reflectivity_sweeps_decide() {
        // Measured angles of the two halves differ slightly; the first
        // collected (lowest number) wins.
        let sweeps = [
            stored(2, 0.48, &["reflectivity", "velocity"]),
            stored(1, 0.53, &["reflectivity"]),
            stored(3, 0.88, &["reflectivity"]),
        ];
        assert_eq!(lowest_reflectivity_cut(None, &sweeps), Some(1));
        // An empty plan counts as no plan.
        let empty = ExtractedVcp {
            number: 0,
            elevations: Vec::new(),
        };
        assert_eq!(lowest_reflectivity_cut(Some(&empty), &sweeps), Some(1));
        // Velocity-only sweeps are not candidates.
        let sweeps = [
            stored(2, 0.5, &["velocity"]),
            stored(3, 0.9, &["reflectivity"]),
        ];
        assert_eq!(lowest_reflectivity_cut(None, &sweeps), Some(3));
        assert_eq!(lowest_reflectivity_cut(None, &[]), None);
    }

    // ── One volume, one set of cells ─────────────────────────────────

    #[wasm_bindgen_test]
    fn every_tilt_of_a_volume_asks_for_the_lowest_cut() {
        let vol = volume(0, &[1, 2, 3, 4, 5]);
        for (elev, product) in [
            (1, "reflectivity"),
            (2, "reflectivity"),
            (4, "reflectivity"),
            (2, "velocity"),
        ] {
            let d = displayed(0, elev, product);
            let key = match decide(&env(Some(&d), Some(&vol)), &StormCells::default()) {
                StormCellAction::Request(key) => key,
                other => panic!("elev {elev} {product}: expected Request, got {other:?}"),
            };
            assert_eq!(key.elevation_number, 1);
            assert_eq!(key.input, CutInput::Stored);
            assert!(!key.fallback);
            assert_eq!(key.scan_key, scan_key(0));
        }
    }

    #[wasm_bindgen_test]
    fn changing_tilt_keeps_the_result_without_rerunning() {
        let vol = volume(0, &[1, 2, 3, 4, 5]);
        let mut state = StormCells::default();
        let d1 = displayed(0, 1, "reflectivity");
        let req = requested(&mut state, &env(Some(&d1), Some(&vol)));
        assert_eq!(
            state.accept(reply(&req, cells(3), true)),
            ReplyDisposition::Cached
        );
        // The next frame shows it.
        assert!(frame(&mut state, &env(Some(&d1), Some(&vol))).is_none());
        assert_eq!(state.result.cells.len(), 3);
        // The volume part follows once; then nothing more is asked.
        let req = requested(&mut state, &env(Some(&d1), Some(&vol)));
        assert_eq!(req.volume_cuts, vec![1, 3, 5]);
        state.accept(reply(&req, with_columns(cells(3)), true));
        assert!(frame(&mut state, &env(Some(&d1), Some(&vol))).is_none());
        assert!(state.result.volume.is_some());
        // Walk the tilt ladder: nothing changes, nothing runs.
        for elev in 2..=5 {
            let d = displayed(0, elev, "reflectivity");
            assert_eq!(
                decide(&env(Some(&d), Some(&vol)), &state),
                StormCellAction::Keep
            );
        }
    }

    #[wasm_bindgen_test]
    fn request_carries_params_position_and_key() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let mut e = env(Some(&d), Some(&vol));
        e.threshold_dbz = 45.0;
        let req = requested(&mut StormCells::default(), &e);
        assert_eq!(req.params.threshold_dbz, 45.0);
        assert_eq!(
            req.params.level_step_dbz,
            DetectionParams::default().level_step_dbz
        );
        assert_eq!((req.radar_lat, req.radar_lon), (32.57, -97.30));
        assert_eq!(req.key.threshold_dbz, 45.0);
    }

    // ── Fallback ─────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn fixed_tilt_fetch_falls_back_to_the_displayed_reflectivity_sweep() {
        // Only cut 3 was fetched.
        let vol = volume(0, &[3]);
        let d = displayed(0, 3, "reflectivity");
        let key = match decide(&env(Some(&d), Some(&vol)), &StormCells::default()) {
            StormCellAction::Request(key) => key,
            other => panic!("expected Request, got {other:?}"),
        };
        assert_eq!(key.elevation_number, 3);
        assert!(key.fallback);

        // Displayed velocity has no reflectivity to fall back on.
        let dv = displayed(0, 3, "velocity");
        assert_eq!(
            decide(&env(Some(&dv), Some(&vol)), &StormCells::default()),
            StormCellAction::Keep
        );
    }

    // ── Cache ────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn empty_result_is_cached_and_final() {
        let vol = volume(0, &[1, 2]);
        let d = displayed(0, 2, "reflectivity");
        let e = env(Some(&d), Some(&vol));
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        state.accept(reply(&req, StormCellsResult::default(), true));
        assert!(frame(&mut state, &e).is_none());
        assert_eq!(state.computed_for.as_ref(), Some(&req.key));
        for _ in 0..300 {
            assert_eq!(decide(&e, &state), StormCellAction::Keep);
        }
    }

    #[wasm_bindgen_test]
    fn returning_to_a_volume_or_threshold_hits_the_cache() {
        let (v0, v1) = (volume(0, &[1]), volume(1, &[1]));
        let (d0, d1) = (
            displayed(0, 1, "reflectivity"),
            displayed(1, 1, "reflectivity"),
        );
        let mut state = StormCells::default();

        let r0 = requested(&mut state, &env(Some(&d0), Some(&v0)));
        state.accept(reply(&r0, cells(2), true));
        frame(&mut state, &env(Some(&d0), Some(&v0)));
        let r1 = requested(&mut state, &env(Some(&d1), Some(&v1)));
        state.accept(reply(&r1, cells(5), true));
        frame(&mut state, &env(Some(&d1), Some(&v1)));
        assert_eq!(state.result.cells.len(), 5);

        // Back to volume 0: shown from the cache, no request.
        let e0 = env(Some(&d0), Some(&v0));
        assert_eq!(decide(&e0, &state), StormCellAction::Show(r0.key.clone()));
        assert!(frame(&mut state, &e0).is_none());
        assert_eq!(state.result.cells.len(), 2);
        assert_eq!(state.timing, Some(timing()));

        // A new threshold misses; the old one hits again.
        let mut e45 = env(Some(&d0), Some(&v0));
        e45.threshold_dbz = 45.0;
        let r45 = requested(&mut state, &e45);
        state.accept(reply(&r45, cells(1), true));
        assert!(matches!(decide(&e0, &state), StormCellAction::Keep));
        assert!(frame(&mut state, &e45).is_none());
        assert_eq!(state.result.cells.len(), 1);
        assert!(frame(&mut state, &e0).is_none());
        assert_eq!(state.result.cells.len(), 2);
    }

    #[wasm_bindgen_test]
    fn cache_is_bounded_least_recently_used_first() {
        let mut state = StormCells::default();
        let n = CACHE_CAPACITY as i64 + 3;
        let vols: Vec<Scan> = (0..n).map(|i| volume(i, &[1])).collect();
        let disp: Vec<DisplayedSweep> = (0..n).map(|i| displayed(i, 1, "reflectivity")).collect();
        for i in 0..n as usize {
            let e = env(Some(&disp[i]), Some(&vols[i]));
            let req = requested(&mut state, &e);
            state.accept(reply(&req, cells(1), true));
            frame(&mut state, &e);
        }
        assert_eq!(state.cache.len(), CACHE_CAPACITY);
        // The oldest three were evicted and need a new run; the fourth is held.
        assert!(matches!(
            decide(&env(Some(&disp[0]), Some(&vols[0])), &state),
            StormCellAction::Request(_)
        ));
        assert!(matches!(
            decide(&env(Some(&disp[3]), Some(&vols[3])), &state),
            StormCellAction::Show(_)
        ));
    }

    // ── Requests in flight and stale replies ─────────────────────────

    #[wasm_bindgen_test]
    fn one_request_in_flight_and_the_previous_cells_stay() {
        let (v0, v1, v2) = (volume(0, &[1]), volume(1, &[1]), volume(2, &[1]));
        let (d0, d1, d2) = (
            displayed(0, 1, "reflectivity"),
            displayed(1, 1, "reflectivity"),
            displayed(2, 1, "reflectivity"),
        );
        let mut state = StormCells::default();
        let r0 = requested(&mut state, &env(Some(&d0), Some(&v0)));
        state.accept(reply(&r0, cells(2), true));
        frame(&mut state, &env(Some(&d0), Some(&v0)));

        // Scrub to volume 1: request; then on to volume 2 while it runs.
        let r1 = requested(&mut state, &env(Some(&d1), Some(&v1)));
        assert_eq!(
            decide(&env(Some(&d2), Some(&v2)), &state),
            StormCellAction::Keep
        );
        assert_eq!(state.result.cells.len(), 2, "previous cells stay meanwhile");

        // Volume 1's reply is cached but not drawn over volume 2.
        state.accept(reply(&r1, cells(7), true));
        let e2 = env(Some(&d2), Some(&v2));
        let r2 = requested(&mut state, &e2);
        assert_eq!(r2.key.scan_key, scan_key(2));
        assert_eq!(state.result.cells.len(), 2);
        assert_eq!(state.computed_for.as_ref(), Some(&r0.key));
    }

    #[wasm_bindgen_test]
    fn reply_after_timeout_or_reset_is_dropped() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        let mut e = env(Some(&d), Some(&vol));
        let first = requested(&mut state, &e);

        // Still waiting just before the timeout; a new request after it.
        e.now_ms += REQUEST_TIMEOUT_MS - 1.0;
        assert_eq!(decide(&e, &state), StormCellAction::Keep);
        e.now_ms += 2.0;
        let second = requested(&mut state, &e);
        assert_ne!(first.request_id, second.request_id);
        assert_eq!(
            state.accept(reply(&first, cells(9), true)),
            ReplyDisposition::Dropped
        );
        assert_eq!(state.fail(first.request_id), ReplyDisposition::Dropped);

        // A site change resets; the reply in flight is dropped.
        state.reset();
        assert_eq!(
            state.accept(reply(&second, cells(9), true)),
            ReplyDisposition::Dropped
        );
        assert!(state.cache.is_empty() && state.computed_for.is_none());
        // Request ids keep increasing across the reset.
        let third = requested(&mut state, &env(Some(&d), Some(&vol)));
        assert!(third.request_id > second.request_id);
    }

    #[wasm_bindgen_test]
    fn awaits_only_the_request_in_flight() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        assert!(!state.awaits(1), "nothing in flight");
        let req = requested(&mut state, &env(Some(&d), Some(&vol)));
        assert!(state.awaits(req.request_id));
        assert!(!state.awaits(req.request_id + 1));
        state.accept(reply(&req, cells(1), true));
        assert!(!state.awaits(req.request_id), "answered");
        let (vol1, d1) = (volume(1, &[1]), displayed(1, 1, "reflectivity"));
        let again = requested(&mut state, &env(Some(&d1), Some(&vol1)));
        state.reset();
        assert!(!state.awaits(again.request_id), "reset drops it");
    }

    #[wasm_bindgen_test]
    fn failure_is_cached_as_empty_and_not_retried() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let e = env(Some(&d), Some(&vol));
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        assert_eq!(state.fail(req.request_id), ReplyDisposition::Cached);
        assert!(frame(&mut state, &e).is_none());
        assert!(state.result.is_empty());
        assert_eq!(state.timing, None);
        assert_eq!(state.result.source.params.threshold_dbz, 35.0);
        for _ in 0..10 {
            assert_eq!(decide(&e, &state), StormCellAction::Keep);
        }
        assert_eq!(
            state.stats().unwrap().label(),
            "0 cells, 0 groups · cut 1 · failed"
        );
    }

    // ── Live ─────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn live_cut_is_provisional_per_chunk_then_final_once_stored() {
        let plan = vcp(&[0.5, 0.5, 0.9]);
        let d = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        let mut progress = LiveCutProgress {
            scan_key: scan_key(0),
            elevation_number: 1,
            radials: 120,
        };

        let r120 = requested(&mut state, &live(&d, &plan, &progress));
        assert_eq!(r120.key.input, CutInput::Live { radials: 120 });
        state.accept(reply(&r120, cells(1), false));
        frame(&mut state, &live(&d, &plan, &progress));
        assert!(state.is_provisional());
        assert!(state.stats().unwrap().label().ends_with("· provisional"));

        // The next chunk extends the cut: a new key, a new provisional run.
        progress.radials = 360;
        let r360 = requested(&mut state, &live(&d, &plan, &progress));
        assert_eq!(r360.key.input, CutInput::Live { radials: 360 });
        state.accept(reply(&r360, cells(3), false));
        frame(&mut state, &live(&d, &plan, &progress));
        assert_eq!(state.result.cells.len(), 3);
        // The 120-radial result was evicted by the newer one.
        assert_eq!(state.cache.len(), 1);

        // The cut completes: the stream moves on to cut 2, the timeline has
        // not listed cut 1 yet. Hold the provisional cells; no fallback run.
        let next = LiveCutProgress {
            scan_key: scan_key(0),
            elevation_number: 2,
            radials: 40,
        };
        let d2 = displayed(0, 2, "reflectivity");
        let mut e = env(Some(&d2), None);
        e.live_vcp = Some(&plan);
        e.live_cut = Some(&next);
        assert_eq!(decide(&e, &state), StormCellAction::Keep);

        // The timeline lists cut 1: the stored cut gives the final result.
        let vol = volume(0, &[1]);
        e.volume = Some(&vol);
        let stored_req = requested(&mut state, &e);
        assert_eq!(stored_req.key.input, CutInput::Stored);
        assert_eq!(stored_req.key.elevation_number, 1);
        state.accept(reply(&stored_req, cells(4), true));
        frame(&mut state, &e);
        assert!(!state.is_provisional());
        assert_eq!(state.result.cells.len(), 4);
        // The stored result evicted the provisional one.
        assert_eq!(state.cache.len(), 1);
    }

    #[wasm_bindgen_test]
    fn live_reply_on_a_finished_cut_is_also_the_stored_result() {
        let plan = vcp(&[0.5, 0.5]);
        let d = displayed(0, 1, "reflectivity");
        let progress = LiveCutProgress {
            scan_key: scan_key(0),
            elevation_number: 1,
            radials: 700,
        };
        let mut e = env(Some(&d), None);
        e.live_vcp = Some(&plan);
        e.live_cut = Some(&progress);
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        // The stream finished the cut before the worker read it.
        state.accept(reply(&req, cells(2), true));
        frame(&mut state, &e);
        assert!(!state.is_provisional());

        // Once the timeline lists the cut, the stored key is a cache hit.
        let vol = volume(0, &[1]);
        let stored_env = env(Some(&d), Some(&vol));
        assert!(matches!(
            decide(&stored_env, &state),
            StormCellAction::Show(_)
        ));
        assert!(frame(&mut state, &stored_env).is_none());
        assert_eq!(state.result.cells.len(), 2);
    }

    #[wasm_bindgen_test]
    fn live_join_after_the_lowest_cut_falls_back_to_the_partial() {
        // Joined during cut 3: cut 1 of this volume will never be stored.
        let plan = vcp(&[0.5, 0.5, 0.9]);
        let d = displayed(0, 3, "reflectivity");
        let progress = LiveCutProgress {
            scan_key: scan_key(0),
            elevation_number: 3,
            radials: 200,
        };
        let mut e = env(Some(&d), None);
        e.live_vcp = Some(&plan);
        e.live_cut = Some(&progress);
        let key = match decide(&e, &StormCells::default()) {
            StormCellAction::Request(key) => key,
            other => panic!("expected Request, got {other:?}"),
        };
        assert_eq!(key.elevation_number, 3);
        assert!(key.fallback);
        assert_eq!(key.input, CutInput::Live { radials: 200 });
    }

    // ── Gates ────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn off_or_blank_clears_but_keeps_the_cache() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let e = env(Some(&d), Some(&vol));
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        state.accept(reply(&req, cells(2), true));
        frame(&mut state, &e);

        let mut off = env(Some(&d), Some(&vol));
        off.visible = false;
        assert_eq!(decide(&off, &state), StormCellAction::Clear);
        assert_eq!(decide(&env(None, None), &state), StormCellAction::Clear);

        frame(&mut state, &off);
        assert!(state.result.is_empty() && state.computed_for.is_none());
        assert_eq!(decide(&off, &state), StormCellAction::Keep);
        // Back on: from the cache, no worker run.
        assert!(matches!(decide(&e, &state), StormCellAction::Show(_)));
    }

    #[wasm_bindgen_test]
    fn cells_stay_over_every_product_in_2d() {
        let (v0, v1) = (volume(0, &[1, 2]), volume(1, &[1, 2]));
        let d0 = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        let req = requested(&mut state, &env(Some(&d0), Some(&v0)));
        state.accept(reply(&req, cells(3), true));
        frame(&mut state, &env(Some(&d0), Some(&v0)));

        // Velocity, correlation coefficient, ... of the same volume: the
        // same cells, no new run.
        for product in ["velocity", "correlation_coefficient", "spectrum_width"] {
            let d = displayed(0, 2, product);
            assert_eq!(
                decide(&env(Some(&d), Some(&v0)), &state),
                StormCellAction::Keep,
                "{product}"
            );
        }
        // Velocity of the next volume: its own lowest reflectivity cut.
        let d1 = displayed(1, 2, "velocity");
        match decide(&env(Some(&d1), Some(&v1)), &state) {
            StormCellAction::Request(key) => {
                assert_eq!(key.scan_key, scan_key(1));
                assert_eq!(key.elevation_number, 1);
                assert!(!key.fallback);
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn globe_defers_until_back_in_2d() {
        let (v0, v1) = (volume(0, &[1]), volume(1, &[1]));
        let (d0, d1) = (
            displayed(0, 1, "reflectivity"),
            displayed(1, 1, "reflectivity"),
        );
        let mut state = StormCells::default();
        let req = requested(&mut state, &env(Some(&d0), Some(&v0)));
        state.accept(reply(&req, cells(1), true));
        frame(&mut state, &env(Some(&d0), Some(&v0)));

        let mut e = env(Some(&d1), Some(&v1));
        e.view_mode = ViewMode::Globe3D;
        assert_eq!(decide(&e, &state), StormCellAction::Keep);
        assert_eq!(decide(&e, &StormCells::default()), StormCellAction::Keep);
        e.view_mode = ViewMode::Flat2D;
        assert!(matches!(decide(&e, &state), StormCellAction::Request(_)));
    }

    #[wasm_bindgen_test]
    fn another_volume_without_a_usable_cut_clears() {
        let vol = volume(0, &[1]);
        let d = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        let req = requested(&mut state, &env(Some(&d), Some(&vol)));
        state.accept(reply(&req, cells(1), true));
        frame(&mut state, &env(Some(&d), Some(&vol)));

        // Same volume, velocity on the GPU, cut 1 unknown to the timeline:
        // keep this volume's cells.
        let dv = displayed(0, 2, "velocity");
        assert_eq!(decide(&env(Some(&dv), None), &state), StormCellAction::Keep);
        // Another volume with nothing usable: clear.
        let other = displayed(1, 2, "velocity");
        assert_eq!(
            decide(&env(Some(&other), None), &state),
            StormCellAction::Clear
        );
    }

    #[wasm_bindgen_test]
    fn stats_label_reads_counts_cut_and_timing() {
        let stats = StormCellStats {
            cells: 5,
            groups: 1,
            elevation_number: 1,
            elevation_deg: Some(0.48),
            timing: Some(timing()),
            provisional: false,
            fallback: false,
        };
        assert_eq!(
            stats.label(),
            "5 cells, 1 group · cut 1 (0.5°) · 80 ms detect / 90 ms total"
        );
        let stats = StormCellStats {
            cells: 1,
            groups: 2,
            fallback: true,
            provisional: true,
            ..stats
        };
        assert_eq!(
            stats.label(),
            "1 cell, 2 groups · cut 1 (0.5°) · 80 ms detect / 90 ms total · displayed tilt · provisional"
        );
    }

    #[wasm_bindgen_test]
    fn threshold_is_clamped_to_slider_range() {
        assert_eq!(clamp_threshold(35.0), 35.0);
        assert_eq!(clamp_threshold(5.0), THRESHOLD_MIN_DBZ);
        assert_eq!(clamp_threshold(90.0), THRESHOLD_MAX_DBZ);
        assert_eq!(clamp_threshold(f32::NAN), THRESHOLD_DEFAULT_DBZ);
        assert_eq!(clamp_threshold(f32::INFINITY), THRESHOLD_MAX_DBZ);
    }

    #[wasm_bindgen_test]
    fn slider_works_over_every_product_with_a_hint_off_reflectivity() {
        assert_eq!(
            threshold_slider(RadarProduct::Reflectivity),
            ThresholdSliderVm {
                enabled: true,
                hint: None
            }
        );
        for &p in RadarProduct::all() {
            if p != RadarProduct::Reflectivity {
                let vm = threshold_slider(p);
                assert!(vm.enabled);
                assert_eq!(vm.hint, Some("Cells from reflectivity"));
            }
        }
    }

    // ── Tracking history ─────────────────────────────────────────────

    fn stored_key(vol: i64) -> StormCellKey {
        StormCellKey {
            scan_key: scan_key(vol),
            elevation_number: 1,
            input: CutInput::Stored,
            fallback: false,
            threshold_dbz: 35.0,
        }
    }

    #[wasm_bindgen_test]
    fn history_is_requested_after_the_shown_volume_nearest_first() {
        let vol = volume(3, &[1, 2]);
        let d = displayed(3, 1, "reflectivity");
        let history = [stored_key(1), stored_key(2)];
        let env = StormCellEnv {
            history: &history,
            ..env(Some(&d), Some(&vol))
        };
        let mut state = StormCells::default();
        // The displayed volume first.
        let req = requested(&mut state, &env);
        assert_eq!(req.key, stored_key(3));
        // Nothing else while it is in flight.
        assert_eq!(decide(&env, &state), StormCellAction::Keep);
        state.accept(reply(&req, cells(1), true));
        assert!(frame(&mut state, &env).is_none(), "shows it");
        assert_eq!(state.computed_for, Some(stored_key(3)));
        // Then the history, nearest volume first, one at a time.
        let req = requested(&mut state, &env);
        assert_eq!(req.key, stored_key(2));
        assert_eq!(decide(&env, &state), StormCellAction::Keep);
        state.accept(reply(&req, cells(2), true));
        let req = requested(&mut state, &env);
        assert_eq!(req.key, stored_key(1));
        state.fail(req.request_id);
        // All cached (the failure as empty): nothing more to do, and the
        // shown result never changed.
        assert_eq!(decide(&env, &state), StormCellAction::Keep);
        assert_eq!(state.computed_for, Some(stored_key(3)));
        assert_eq!(state.result.cells.len(), 1);
        assert_eq!(
            state.cached_result(&stored_key(1)),
            Some(&StormCellsResult {
                source: crate::storm_cells::DetectionSource {
                    params: DetectionParams::default(),
                    ..Default::default()
                },
                ..StormCellsResult::default()
            })
        );
    }

    #[wasm_bindgen_test]
    fn a_new_shown_volume_goes_ahead_of_history() {
        let vol4 = volume(4, &[1]);
        let d4 = displayed(4, 1, "reflectivity");
        let mut state = StormCells::default();
        // Volume 3 is shown and its history never asked for; the user
        // scrubs to volume 4.
        state.seed(stored_key(3), cells(1), true, true);
        let history = [stored_key(2), stored_key(3)];
        let env4 = StormCellEnv {
            history: &history,
            ..env(Some(&d4), Some(&vol4))
        };
        assert_eq!(requested(&mut state, &env4).key, stored_key(4));
    }

    #[wasm_bindgen_test]
    fn no_history_is_requested_without_tracks() {
        let vol = volume(3, &[1]);
        let d = displayed(3, 1, "reflectivity");
        let mut state = StormCells::default();
        state.seed(stored_key(3), cells(1), true, true);
        assert_eq!(
            decide(&env(Some(&d), Some(&vol)), &state),
            StormCellAction::Keep
        );
    }

    // ── Volume ──────────────────────────────────────────────────────

    /// `result` as a volume reply carries it: a column per cell.
    fn with_columns(mut result: StormCellsResult) -> StormCellsResult {
        result.volume = Some(crate::storm_cells::VolumeStructure {
            tilts: Vec::new(),
            cells: result
                .cells
                .iter()
                .map(|_| crate::storm_cells::CellColumn {
                    echo_top_km: Some(12.0),
                    ..Default::default()
                })
                .collect(),
            // The worker does not know the plan.
            complete: false,
        });
        result
    }

    #[wasm_bindgen_test]
    fn volume_cuts_take_one_cut_per_tilt_and_know_the_plan() {
        // Split cuts at 0.5 and 0.9, then 1.3, all stored.
        let vol = volume(0, &[1, 2, 3, 4, 5]);
        let all = volume_cuts(vol.vcp_pattern.as_ref(), &vol.sweeps);
        assert_eq!(
            all,
            VolumeCuts {
                cuts: vec![1, 3, 5],
                complete: true
            }
        );
        // Only the Doppler half of 0.9 stored, 1.3 missing: partial.
        let vol = volume(0, &[1, 2, 4]);
        let part = volume_cuts(vol.vcp_pattern.as_ref(), &vol.sweeps);
        assert_eq!(part.cuts, vec![1, 4]);
        assert!(!part.complete);
        // A SAILS rescan of the lowest tilt counts once, as the first cut.
        let plan = vcp(&[0.5, 0.5, 0.9, 0.5, 1.8]);
        let sweeps = [
            stored(1, 0.48, &["reflectivity"]),
            stored(3, 0.91, &["reflectivity"]),
            stored(4, 0.52, &["reflectivity"]),
            stored(5, 1.79, &["reflectivity"]),
            stored(2, 0.5, &["velocity"]),
        ];
        let sails = volume_cuts(Some(&plan), &sweeps);
        assert_eq!(sails.cuts, vec![1, 3, 5]);
        assert!(sails.complete);
        // Without a plan the stored tilts are the volume.
        let none = volume_cuts(None, &sweeps);
        assert_eq!(none.cuts, vec![1, 3, 5]);
        assert!(none.complete);
        assert!(volume_cuts(None, &[]).cuts.is_empty());
    }

    #[wasm_bindgen_test]
    fn the_volume_part_follows_the_cells_and_is_attached_to_them() {
        let vol = volume(0, &[1, 2, 3, 4, 5]);
        let d = displayed(0, 1, "reflectivity");
        let e = env(Some(&d), Some(&vol));
        let mut state = StormCells::default();
        // The cells first, alone: their latency is unchanged.
        let req = requested(&mut state, &e);
        assert!(req.volume_cuts.is_empty());
        state.accept(reply(&req, cells(2), true));
        assert!(frame(&mut state, &e).is_none(), "shows the cells");
        assert_eq!(state.result.volume, None);
        // Then the volume, for the same key, one at a time.
        let req = requested(&mut state, &e);
        assert_eq!(req.key, stored_key(0));
        assert_eq!(req.volume_cuts, vec![1, 3, 5]);
        assert_eq!(decide(&e, &state), StormCellAction::Keep, "in flight");
        assert_eq!(
            state.accept(reply(&req, with_columns(cells(2)), true)),
            ReplyDisposition::Cached
        );
        // The next frame shows the columns, marked complete from the plan.
        assert_eq!(decide(&e, &state), StormCellAction::Show(stored_key(0)));
        assert!(frame(&mut state, &e).is_none());
        let v = state.result.volume.as_ref().expect("columns shown");
        assert!(v.complete);
        assert_eq!(v.cells.len(), 2);
        assert_eq!(state.result.cells, cells(2).cells);
        // Nothing more to ask.
        assert_eq!(decide(&e, &state), StormCellAction::Keep);
        // Away and back: the cached result carries its columns.
        state.clear();
        assert!(frame(&mut state, &e).is_none());
        assert!(state.result.volume.is_some());
    }

    #[wasm_bindgen_test]
    fn a_partial_volume_is_asked_again_as_cuts_arrive() {
        let d = displayed(0, 1, "reflectivity");
        let mut state = StormCells::default();
        // Only the lowest tilt: no volume request.
        let vol = volume(0, &[1, 2]);
        let req = requested(&mut state, &env(Some(&d), Some(&vol)));
        state.accept(reply(&req, cells(1), true));
        assert!(frame(&mut state, &env(Some(&d), Some(&vol))).is_none());
        assert_eq!(
            decide(&env(Some(&d), Some(&vol)), &state),
            StormCellAction::Keep
        );
        // 0.9 lands: a provisional column.
        let vol = volume(0, &[1, 2, 3]);
        let req = requested(&mut state, &env(Some(&d), Some(&vol)));
        assert_eq!(req.volume_cuts, vec![1, 3]);
        state.accept(reply(&req, with_columns(cells(1)), true));
        assert!(frame(&mut state, &env(Some(&d), Some(&vol))).is_none());
        assert!(!state.result.volume.as_ref().unwrap().complete);
        assert_eq!(
            decide(&env(Some(&d), Some(&vol)), &state),
            StormCellAction::Keep
        );
        // 1.3 lands: asked again, and the columns become final.
        let vol = volume(0, &[1, 2, 3, 5]);
        let req = requested(&mut state, &env(Some(&d), Some(&vol)));
        assert_eq!(req.volume_cuts, vec![1, 3, 5]);
        state.accept(reply(&req, with_columns(cells(1)), true));
        assert!(frame(&mut state, &env(Some(&d), Some(&vol))).is_none());
        assert!(state.result.volume.as_ref().unwrap().complete);
    }

    #[wasm_bindgen_test]
    fn a_failed_or_mismatched_volume_keeps_the_cells_and_is_not_retried() {
        let vol = volume(0, &[1, 3, 5]);
        let d = displayed(0, 1, "reflectivity");
        let e = env(Some(&d), Some(&vol));
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        state.accept(reply(&req, cells(2), true));
        frame(&mut state, &e);
        let req = requested(&mut state, &e);
        assert!(!req.volume_cuts.is_empty());
        assert_eq!(state.fail(req.request_id), ReplyDisposition::Cached);
        assert!(frame(&mut state, &e).is_none(), "not retried");
        assert_eq!(state.result.cells.len(), 2, "cells kept");
        assert_eq!(state.result.volume, None);

        // A reply whose cells are not the cached ones is not attached.
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        state.accept(reply(&req, cells(2), true));
        frame(&mut state, &e);
        let req = requested(&mut state, &e);
        state.accept(reply(&req, with_columns(cells(3)), true));
        assert!(frame(&mut state, &e).is_none());
        assert_eq!(state.result.cells.len(), 2);
        assert_eq!(state.result.volume, None);
        assert_eq!(decide(&e, &state), StormCellAction::Keep);
    }

    #[wasm_bindgen_test]
    fn only_the_displayed_stored_cut_gets_a_volume_and_it_goes_before_history() {
        let vol = volume(3, &[1, 3, 5]);
        let d = displayed(3, 1, "reflectivity");
        let history = [stored_key(2)];
        let e = StormCellEnv {
            history: &history,
            ..env(Some(&d), Some(&vol))
        };
        let mut state = StormCells::default();
        let req = requested(&mut state, &e);
        state.accept(reply(&req, cells(1), true));
        frame(&mut state, &e);
        // Volume before history.
        let req = requested(&mut state, &e);
        assert_eq!((req.key.clone(), req.volume_cuts.len()), (stored_key(3), 3));
        state.accept(reply(&req, with_columns(cells(1)), true));
        frame(&mut state, &e);
        // The history volume is detected without a volume part, and never
        // gets one.
        let req = requested(&mut state, &e);
        assert_eq!(req.key, stored_key(2));
        assert!(req.volume_cuts.is_empty());
        state.accept(reply(&req, cells(1), true));
        assert_eq!(decide(&e, &state), StormCellAction::Keep);
        assert_eq!(state.cached_result(&stored_key(2)).unwrap().volume, None);

        // A live cut or the displayed-tilt fallback gets no volume part.
        let plan = vcp(&[0.5, 0.9, 1.3]);
        let progress = LiveCutProgress {
            scan_key: scan_key(5),
            elevation_number: 1,
            radials: 200,
        };
        let dl = displayed(5, 1, "reflectivity");
        let mut state = StormCells::default();
        let req = requested(&mut state, &live(&dl, &plan, &progress));
        state.accept(reply(&req, cells(1), false));
        frame(&mut state, &live(&dl, &plan, &progress));
        assert_eq!(
            decide(&live(&dl, &plan, &progress), &state),
            StormCellAction::Keep
        );
        let fixed = volume(6, &[3, 5]);
        let df = displayed(6, 3, "reflectivity");
        let mut state = StormCells::default();
        let req = requested(&mut state, &env(Some(&df), Some(&fixed)));
        assert!(req.key.fallback);
        state.accept(reply(&req, cells(1), true));
        frame(&mut state, &env(Some(&df), Some(&fixed)));
        assert_eq!(
            decide(&env(Some(&df), Some(&fixed)), &state),
            StormCellAction::Keep
        );
    }
}
